#!/usr/bin/env bash
# Banc d'essai : exécute le scénario d'intégration en conditions réelles, sans
# aucun sudo sur la machine hôte et sans y monter quoi que ce soit.
#
# Le scénario a besoin d'un vrai btrfs, donc d'un montage, donc de root. Plutôt
# que de demander le mot de passe de l'utilisateur, on fait tourner l'ensemble
# dans un conteneur docker-in-docker privilégié : btrfs réel, démon docker
# imbriqué, conteneurs postgres et nginx réels. L'hôte ne voit qu'un conteneur.
#
#   tests/lab.sh up       crée le banc : docker imbriqué, btrfs-progs, un utilisateur dev
#   tests/lab.sh run      compile ramet, l'installe avec install.sh (qui lance
#                         ramet setup), redimensionne le volume, puis doctor +
#                         scénario (ses options vont au scénario : run --upto 3)
#   tests/lab.sh shell    ouvre un shell dans le banc
#   tests/lab.sh down     supprime le banc
#
# Prérequis sur l'hôte : docker utilisable sans sudo, et la cible Rust musl
# (`rustup target add x86_64-unknown-linux-musl`) : le banc tourne sous Alpine,
# sans glibc, et reçoit donc un binaire statique. Les tests unitaires, eux, se
# passent de btrfs et de docker : ils tournent sur l'hôte (`cargo test`).
set -euo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
LAB=${RAMET_LAB_NAME:-ramet-lab}
IMAGE=${RAMET_LAB_IMAGE:-docker:dind}
MNT=/srv/ramet
TARGET=x86_64-unknown-linux-musl
# Le binaire compilé (hors de /tmp, un tmpfs où `docker cp` n'écrit pas), puis
# là où install.sh le range pour dev.
BUILT=/opt/ramet
BIN=/home/dev/.local/bin/ramet
# L'image de données de dev vit ici plutôt que dans son home : un chemin que
# nulle machine réelle n'utilise, que le scénario exige avant de tout détruire.
DATA_HOME=/var/lib/ramet-lab
IMG=$DATA_HOME/ramet/data.img

die()  { printf '\033[31merreur:\033[0m %s\n' "$*" >&2; exit 1; }
info() { printf '\033[36m::\033[0m %s\n' "$*"; }
ok()   { printf '\033[32m✓\033[0m %s\n' "$*"; }

vivant() { [ "$(docker inspect -f '{{.State.Running}}' "$LAB" 2>/dev/null)" = true ]; }
dans()   { docker exec "$LAB" sh -c "$1"; }
# Le banc ne prouverait pas que ramet se passe de privilèges s'il tournait en root.
en_dev() { docker exec -u dev -e XDG_DATA_HOME="$DATA_HOME" "$LAB" sh -c "$1"; }
# L'administrateur du banc, avec l'image de dev : ce que ferait sudo.
en_root() { docker exec -e XDG_DATA_HOME="$DATA_HOME" "$LAB" sh -c "$1"; }

cmd_up() {
  command -v docker >/dev/null || die "docker introuvable sur l'hôte"
  docker info >/dev/null 2>&1 || die "le démon docker de l'hôte est injoignable"
  if vivant; then info "banc déjà debout : $LAB"; cmd_status; return 0; fi

  docker rm -f "$LAB" >/dev/null 2>&1 || true
  info "démarrage du banc ($IMAGE, privilégié)"
  # --privileged est indispensable : il faut CAP_SYS_ADMIN pour monter un btrfs
  # et créer un périphérique loop. Le conteneur est jetable et local.
  docker run -d --privileged --name "$LAB" -e DOCKER_TLS_CERTDIR= "$IMAGE" >/dev/null

  info "attente du démon docker imbriqué"
  local i
  for i in $(seq 1 60); do
    dans 'docker info' >/dev/null 2>&1 && break
    [ "$i" = 60 ] && die "le démon imbriqué n'a jamais démarré"
    sleep 1
  done
  ok "docker imbriqué : $(dans 'docker version --format "{{.Server.Version}}"')"

  info "installation des prérequis"
  # `sudo` n'est volontairement pas installé : ramet ne doit jamais en avoir besoin.
  dans 'apk add --no-cache python3 git btrfs-progs curl bash util-linux' >/dev/null 2>&1
  dans 'adduser -D -u 1000 dev 2>/dev/null; chown root:dev /var/run/docker.sock && chmod 660 /var/run/docker.sock'

  info "création d'un périphérique loop dans le conteneur"
  # `losetup -f` annonce un numéro libre côté noyau, mais le nœud n'existe pas
  # dans le /dev du conteneur — il le signale par « (lost) ».
  dans '
    [ -e /dev/loop-control ] || mknod /dev/loop-control c 10 237
    libre=$(losetup -f 2>/dev/null | awk "{print \$1}")
    [ -n "$libre" ] || exit 1
    [ -e "$libre" ] || mknod "$libre" b 7 "${libre#/dev/loop}"
  ' || die "aucun périphérique loop disponible"

  # Ce que ramet setup ne fait pas et qu'une vraie distribution fournit :
  # `mount` setuid, sans quoi un utilisateur ne monte pas même une ligne `user`.
  dans 'chmod u+s "$(command -v mount)" "$(command -v umount)"'
  dans "mkdir -p $DATA_HOME && chown dev:dev $DATA_HOME"
  ok "banc prêt : le volume de données viendra de ramet setup"
  cmd_status
}

cmd_sync() {
  vivant || die "banc éteint — lance d'abord : tests/lab.sh up"
  info "copie du dépôt dans le banc"
  dans 'rm -rf /work/ramet && mkdir -p /work/ramet'
  # Les artefacts de compilation pèsent des centaines de Mo et ne servent à
  # rien dans le banc : on n'y copie que les sources.
  tar -C "$REPO" --exclude=./target -cf - . | docker exec -i "$LAB" tar -C /work/ramet -xf -
  # Les fichiers arrivent avec l'uid de l'hôte ; git refuserait un dépôt qu'il
  # juge « dubious ownership ».
  dans 'chown -R dev:dev /work'
  en_dev 'git config --global --add safe.directory "*"'
  dans 'rm -rf /work/ramet/tests/.tmp'
  ok "dépôt à jour dans /work/ramet"
}

cmd_build() {
  vivant || die "banc éteint — lance d'abord : tests/lab.sh up"
  info "compilation de ramet (binaire statique $TARGET)"
  (cd "$REPO" && cargo build --release --locked --quiet --target "$TARGET") \
    || die "la compilation échoue — cible absente ? rustup target add $TARGET"
  docker cp "$REPO/target/$TARGET/release/ramet" "$LAB:$BUILT" >/dev/null
  dans "chmod 755 $BUILT"
  ok "ramet compilé : $BUILT"
}

# install.sh comme le lancerait un utilisateur, puis ramet setup. Le banc n'a
# pas sudo : setup affiche les étapes root au lieu de les jouer. L'administrateur
# du banc ajoute la ligne fstab ; dev monte alors le volume ; ce qui demande
# encore root (les quotas btrfs), ramet le joue lui-même, lancé en root. Un
# dernier setup, en tant que dev, doit trouver tout en place.
cmd_install() {
  info "install.sh, en tant que dev"
  if en_dev "cd /work/ramet && RAMET_BINARY=$BUILT sh install.sh"; then
    return 0
  fi
  info "étapes root jouées par l'administrateur du banc"
  local line
  line=$(en_dev "$BIN doctor --print-fstab")
  dans "mkdir -p $MNT && { grep -qsF ' $MNT ' /etc/fstab || printf '%s\n' '$line' >> /etc/fstab; }"
  en_dev "$BIN setup" >/dev/null 2>&1 || en_root "$BIN setup" >/dev/null || die "ramet setup échoue en root"
  en_dev "$BIN setup" || die "ramet setup échoue encore après les étapes root"
}

# Taille du volume vue par dev, en Gio.
taille() {
  en_dev "$BIN df --json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["size"] >> 30)'
}
taille_attendue() { # <Gio>
  [ "$(taille)" = "$1" ] || die "volume de $(taille) Gio, $1 Gio attendus"
  ok "volume de $1 Gio"
}

# ramet setup --size : la seule autre étape qui demande root. Sans sudo, dev
# obtient les commandes ; l'administrateur du banc les fait jouer par ramet
# lui-même, en root, sur de vraies données. Le volume finit à sa taille de
# départ : un second `run` repart du même état.
cmd_resize() {
  info "ramet setup --size : agrandir puis réduire le volume, en ligne"
  local depart plus out
  depart=$(taille); plus=$((depart + 2))
  en_dev "mkdir -p $MNT/lab && head -c 50000000 /dev/urandom > $MNT/lab/témoin && md5sum $MNT/lab/témoin > /tmp/témoin.md5"
  out=$(en_dev "$BIN setup --size ${plus}G" 2>&1) && die "sans sudo, setup --size aurait dû s'arrêter"
  echo "$out" | grep -q "losetup -c" || die "setup --size n'affiche pas les commandes root :
$out"
  ok "sans sudo : les commandes root sont affichées, rien n'est lancé"
  taille_attendue "$depart"
  en_root "$BIN setup --size ${plus}G" >/dev/null || die "l'agrandissement à ${plus}G échoue en root"
  taille_attendue "$plus"
  en_root "$BIN setup --size 1G" >/dev/null 2>&1 && die "une réduction sous l'espace utilisé + 1 Gio aurait dû être refusée"
  taille_attendue "$plus"
  ok "réduction trop forte refusée, rien n'a bougé"
  en_root "$BIN setup --size ${depart}G" >/dev/null || die "la réduction à ${depart}G échoue en root"
  taille_attendue "$depart"
  en_dev "md5sum -c /tmp/témoin.md5 >/dev/null" || die "le témoin n'a pas survécu aux redimensionnements"
  ok "témoin de 50 Mo intact"
  en_dev "rm -r $MNT/lab"
}

cmd_run() {
  cmd_sync
  cmd_build
  echo
  cmd_install
  echo
  cmd_resize
  echo
  info "ramet doctor (en tant que dev, sans aucun privilège)"
  en_dev "cd /work/ramet && $BIN doctor" || die "doctor signale une erreur"
  echo
  info "scénario d'intégration"
  en_dev "cd /work/ramet && RAMET_BIN=$BIN RAMET_TEST_IMG=$IMG tests/scenario.sh $*"
}

cmd_shell()  { vivant || die "banc éteint"; docker exec -it "$LAB" sh -c 'cd /work/ramet 2>/dev/null; exec sh'; }
cmd_down()   { docker rm -f "$LAB" >/dev/null 2>&1 && info "banc supprimé" || info "aucun banc à supprimer"; }
cmd_status() {
  vivant || { echo "banc éteint ($LAB)"; return 0; }
  echo "banc    : $LAB"
  echo "btrfs   : $(dans "findmnt -no SOURCE,FSTYPE --mountpoint $MNT" 2>/dev/null || echo 'non monté')"
  # busybox df ne connaît pas --output
  echo "espace  : $(dans "df -h $MNT 2>/dev/null | tail -1" | tr -s ' ')"
  echo "envs    : $(dans "ls -1 $MNT/*/ -d 2>/dev/null | wc -l") projet(s) sous $MNT"
}

case "${1:-status}" in
  up)     cmd_up ;;
  sync)   cmd_sync ;;
  build)  cmd_build ;;
  run)    shift; cmd_run "$@" ;;
  shell)  cmd_shell ;;
  down)   cmd_down ;;
  status) cmd_status ;;
  *) die "usage: $0 {up|sync|build|run|shell|down|status}" ;;
esac
