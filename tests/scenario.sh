#!/usr/bin/env bash
# Scénario de référence : ramet de bout en bout, sur du vrai btrfs et du vrai docker.
# Échoue bruyamment à la première étape qui ne passe pas.
#
#   tests/scenario.sh              toutes les étapes
#   tests/scenario.sh --upto 3     s'arrête après l'étape 3 (implémentation incrémentale)
#   tests/scenario.sh --clean      nettoie et sort
#
# Se lance dans le banc : tests/lab.sh run [--upto N]. Le banc installe ramet,
# dont `ramet setup` prépare le btrfs jetable monté sur /srv/ramet ;
# RAMET_BIN=<chemin> désigne un autre binaire que le ramet du PATH.
set -euo pipefail
set -E   # sans errtrace, le trap ERR ci-dessous ne voit rien de ce qui se passe dans une fonction

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
RAMET=${RAMET_BIN:-$(command -v ramet || true)}
ROOT=${RAMET_ROOT:-/srv/ramet}
TMP=$REPO/tests/.tmp
PROJ=$TMP/example          # clone principal = env "main"
WT=$TMP/example.wt         # worktrees, à côté du clone
DATA=$ROOT/example         # volume btrfs du projet

UPTO=99
case "${1:-}" in
  --upto) UPTO=${2:?--upto attend un numéro} ;;
  --clean) UPTO=0 ;;
  "") ;;
  *) echo "usage: $0 [--upto N | --clean]" >&2; exit 2 ;;
esac

# ---------------------------------------------------------------- affichage --
B=$'\033[1m'; R=$'\033[31m'; G=$'\033[32m'; Y=$'\033[33m'; C=$'\033[36m'; Z=$'\033[0m'
STEP=setup
step() {
  STEP=$1
  [ "$1" -le "$UPTO" ] 2>/dev/null || { echo; echo "${Y}— arrêt demandé avant l'étape $1 —${Z}"; finish; exit 0; }
  echo; echo "${B}${C}══ étape $1 ${Z}${B}$2${Z}"
}
ok()   { echo "   ${G}✓${Z} $*"; }
info() { echo "   ${C}·${Z} $*"; }
die()  { echo "   ${R}✗ $*${Z}" >&2; exit 1; }
trap 'echo; echo "${R}${B}ÉCHEC à l'"'"'étape $STEP${Z} (ligne $LINENO)" >&2' ERR

need() { command -v "$1" >/dev/null || die "$1 introuvable"; }
need_ramet() {
  [ -n "$RAMET" ] && [ -x "$RAMET" ] || die "binaire ramet introuvable : ${RAMET:-absent du PATH}
     (tests/lab.sh run l'installe dans le banc ; RAMET_BIN=<chemin> en désigne un autre)"
}

# --------------------------------------------------------------- assertions --
assert_eq() {  # <attendu> <obtenu> <libellé>
  [ "$1" = "$2" ] || die "$3 : attendu «$1», obtenu «$2»"
  ok "$3 = $1"
}
assert_dir()   { [ -d "$1" ] || die "dossier absent : $1"; ok "dossier présent : $1"; }
assert_nodir() { [ ! -e "$1" ] || die "devrait avoir disparu : $1"; ok "absent comme attendu : $1"; }
# La racine d'un sous-volume btrfs a toujours l'inode 256 : testable sans
# privilège, contrairement à `btrfs subvolume show`, qui exige root.
is_subvol() { [ -d "$1" ] && [ "$(stat -c %i "$1" 2>/dev/null)" = 256 ]; }
assert_subvol() {
  is_subvol "$1" || die "pas un sous-volume btrfs : $1"
  ok "sous-volume : $1"
}
assert_nosubvol() {
  [ ! -e "$1" ] || die "sous-volume encore là : $1"; ok "sous-volume supprimé : $1"
}

# ------------------------------------------------------------------- outils --
# Exécute une commande dans un répertoire donné.
inn() { local d=$1; shift; (cd "$d" && "$@"); }

# ramet lancé depuis le worktree $1.
rmt() { local d=$1; shift; inn "$d" "$RAMET" "$@"; }

# psql. $1 = worktree, $2 = "ramet" ou "plain", $3 = SQL. Sort le résultat brut.
psql_via() {
  local d=$1 mode=$2 sql=$3
  if [ "$mode" = ramet ]; then
    rmt "$d" compose exec -T db psql -U postgres -d example -Atc "$sql"
  else
    inn "$d" docker compose exec -T db psql -U postgres -d example -Atc "$sql"
  fi
}

wait_pg() {  # <worktree> <mode>
  # `pg_isready` (et le healthcheck de l'image postgres, qui s'en sert) répond
  # « prêt » pendant le serveur temporaire lancé par initdb au tout premier
  # démarrage. Le psql suivant tombe alors sur « the database system is
  # shutting down ». On exige donc une vraie requête aboutie.
  local d=$1 mode=$2 i
  for i in $(seq 1 90); do
    psql_via "$d" "$mode" "select 1" >/dev/null 2>&1 && return 0
    sleep 1
  done
  die "postgres jamais prêt dans $d ($mode)"
}

init_table() { psql_via "$1" "$2" "create table if not exists notes(id serial primary key, txt text unique);" >/dev/null; }
add_note()   { psql_via "$1" "$2" "insert into notes(txt) values ('$3') on conflict do nothing;" >/dev/null; info "note ajoutée : $3"; }
count_note() { psql_via "$1" "$2" "select count(*) from notes where txt='$3';" | tr -d '[:space:]'; }
notes()      { psql_via "$1" "$2" "select txt from notes order by txt;" | tr '\n' ' '; }

has_note() {  # <wt> <mode> <txt> <libellé>
  assert_eq 1 "$(count_note "$1" "$2" "$3")" "${4:-note «$3» présente}"
}
lacks_note() {
  assert_eq 0 "$(count_note "$1" "$2" "$3")" "${4:-note «$3» absente}"
}

# Écrit un témoin dans le volume nommé `uploads` et vérifie que nginx le sert.
put_upload() { rmt "$1" compose exec -T web sh -c "mkdir -p /usr/share/nginx/html/uploads && printf '%s' '$2' > /usr/share/nginx/html/uploads/marker.txt"; info "témoin uploads : $2"; }
get_upload() { rmt "$1" compose exec -T web cat /usr/share/nginx/html/uploads/marker.txt 2>/dev/null || true; }

# Port hôte publié pour <service>:<cport> dans l'env <nom>, lu dans ramet ls --json.
host_port() { # <worktree> <env> <service:cport>
  rmt "$1" ls --json | python3 -c '
import json,sys
envs=json.load(sys.stdin)
envs=envs.get("envs",envs) if isinstance(envs,dict) else envs
for e in envs:
    if e["name"]==sys.argv[1]:
        print(e["ports"]["map"][sys.argv[2]]); break
else:
    sys.exit("env %s absent de ls --json" % sys.argv[1])' "$2" "$3"
}

http_body() { curl -fsS --max-time 5 "$1"; }

# ------------------------------------------------------------------ ménage --
cleanup() {
  echo "${Y}── nettoyage de l'état précédent ──${Z}"
  # 1. stacks compose (classique + tous les envs ramet)
  for p in $(docker compose ls -aq --filter name='^example' 2>/dev/null || true); do
    info "docker compose -p $p down -v"
    docker compose -p "$p" down -v --remove-orphans -t 2 >/dev/null 2>&1 || true
  done
  # Les volumes des envs ramet sont adossés à un bind : `docker volume rm` retire
  # l objet docker sans toucher aux données, que le sous-volume emporte ensuite.
  local vols
  vols=$(docker volume ls -q --filter name='^example[-_]' 2>/dev/null || true)
  if [ -n "$vols" ]; then
    info "docker volume rm : $(echo "$vols" | tr '\n' ' ')"
    echo "$vols" | xargs -r docker volume rm -f >/dev/null 2>&1 || true
  fi

  # 2. worktrees puis clone
  #    `|| true` obligatoire : un grep sans résultat renvoie 1 et, sous
  #    `set -e` + `pipefail`, ferait sortir le script alors qu'il n'y a rien à faire.
  local w
  if [ -d "$PROJ/.git" ]; then
    for w in $(inn "$PROJ" git worktree list --porcelain 2>/dev/null \
                 | awk '/^worktree /{print $2}' | grep -v "^$PROJ\$" || true); do
      info "git worktree remove $w"
      inn "$PROJ" git worktree remove --force "$w" >/dev/null 2>&1 || rm -rf "$w"
    done
  fi

  # 3. sous-volumes btrfs — uniquement sous $DATA, vérifié deux fois
  if [ -d "$DATA" ]; then
    case "$DATA" in
      "$ROOT"/example) : ;;
      *) die "refus de nettoyer $DATA (hors de $ROOT/example)" ;;
    esac
    # tri inverse : les checkpoints (`env@label`) passent avant leur env
    local victims=() p
    for p in $(ls -1 "$DATA" 2>/dev/null | sort -r || true); do
      is_subvol "$DATA/$p" && victims+=("$DATA/$p")
    done
    if [ ${#victims[@]} -gt 0 ]; then
      # Aucun sudo : tout passe sans privilège. Un checkpoint est
      # read-only, il faut lui retirer la propriété `ro` avant de le supprimer.
      for p in "${victims[@]}"; do
        [ -d "$p" ] || continue
        case "$p" in "$ROOT"/example/*) : ;; *) die "refus de supprimer $p" ;; esac
        info "btrfs subvolume delete $p"
        btrfs property set "$p" ro false >/dev/null 2>&1 || true
        btrfs subvolume delete "$p" >/dev/null
      done
    fi
    rmdir "$DATA" 2>/dev/null || rm -rf "$DATA"
  fi

  rm -rf "$TMP"
  echo "${G}nettoyé.${Z}"
}

finish() {
  echo
  echo "${B}Reste en place :${Z} $TMP, les stacks docker, $DATA"
  echo "Pour tout jeter : ${C}tests/scenario.sh --clean${Z}"
}

# ================================================================== préambule =
need docker; need git; need python3; need curl; need btrfs
docker compose version >/dev/null || die "docker compose v2 requis"

findmnt -no FSTYPE --mountpoint "$ROOT" 2>/dev/null | grep -qx btrfs \
  || die "$ROOT n'est pas un montage btrfs. Le scénario se lance dans le banc : tests/lab.sh up, puis tests/lab.sh run"
[ -w "$ROOT" ] || die "$ROOT n'est pas accessible en écriture par $(id -un) — voir tests/lab.sh"

# Garde-fou : le scénario détruit tout sous $DATA et arrête les stacks du projet.
# Depuis que ramet monte lui-même un volume de données sur $ROOT à la demande,
# rien ne garantit plus que ce qui est monté soit le loop jetable. On vérifie.
SRC=$(findmnt -no SOURCE --mountpoint "$ROOT" 2>/dev/null || true)
BACK=$(losetup -nO BACK-FILE "$SRC" 2>/dev/null || true)
IMG=/var/lib/ramet/data.img
[ -e /etc/ramet-lab ] || die "pas de /etc/ramet-lab : ce n'est pas le banc de test.
     Le scénario détruit tout sous $DATA : il refuse de tourner sur un volume
     de données réel. Lance-le dans le banc : tests/lab.sh run."
[ "$BACK" = "$IMG" ] || die "$ROOT n'est pas alimenté par l'image du banc.
     monté depuis : ${BACK:-$SRC}
     attendu      : $IMG
     Le scénario détruit tout sous $DATA : il refuse de tourner sur un volume
     de données réel. Lance-le dans le banc : tests/lab.sh run."
ok "btrfs de test monté sur $ROOT (depuis $IMG)"

cleanup
[ "$UPTO" -eq 0 ] && exit 0

echo; echo "${B}${C}══ préparation${Z}${B} clone principal jetable${Z}"
mkdir -p "$TMP"
cp -a "$REPO/example" "$PROJ"
echo ".env" > "$PROJ/.gitignore"
inn "$PROJ" git init -q -b main
inn "$PROJ" git add -A
inn "$PROJ" git -c user.name=ramet -c user.email=ramet@test commit -qm "projet d'exemple"
# Un .env local, ignoré par git, qui pointe sur le port publié de main.
echo "WEB_URL=http://localhost:8080/uploads/marker.txt" > "$PROJ/.env"
ok "dépôt git autonome : $PROJ (branche main), .env local non suivi"
info "worktrees iront dans $WT ; données dans $DATA"

# ====================================================================== 1 =====
step 1 "docker compose classique : la stack marche sans ramet"
inn "$PROJ" docker compose up -d --wait
wait_pg "$PROJ" plain
init_table "$PROJ" plain
add_note "$PROJ" plain r1-classique
has_note "$PROJ" plain r1-classique
assert_eq "coucou-classique" "$(inn "$PROJ" docker compose exec -T web sh -c \
  "mkdir -p /usr/share/nginx/html/uploads && printf coucou-classique > /usr/share/nginx/html/uploads/marker.txt && cat /usr/share/nginx/html/uploads/marker.txt")" \
  "témoin écrit dans le volume uploads"
assert_eq "coucou-classique" "$(http_body http://localhost:8080/uploads/marker.txt)" "nginx sert le témoin sur 8080"
ok "le projet d'exemple fonctionne sans ramet"

# ====================================================================== 2 =====
step 2 "ramet init : migration des volumes, données conservées"
need_ramet
rmt "$PROJ" init
assert_subvol "$DATA/main"
assert_dir "$DATA/main/volumes/pgdata"
assert_dir "$DATA/main/volumes/uploads"
[ -f "$DATA/main/env.json" ] || die "env.json manquant"
ok "env.json écrit"
# pgdata est chowné 70:70 mode 0700 par postgres : illisible pour un compte
# ordinaire. On vérifie donc autrement.
assert_eq "$DATA/main/volumes/pgdata" \
  "$(docker volume inspect example-main_pgdata --format '{{.Options.device}}')" \
  "le volume docker de main pointe sur le sous-volume btrfs"
assert_eq "coucou-classique" "$(cat "$DATA/main/volumes/uploads/marker.txt" 2>/dev/null)" \
  "le témoin uploads est bien sur disque dans le sous-volume"
rmt "$PROJ" compose exec -T db test -f /var/lib/postgresql/data/PG_VERSION \
  || die "pas de cluster postgres dans le datadir monté"
ok "cluster postgres présent dans le datadir migré"
assert_eq "example-main_default" \
  "$(docker network ls --format '{{.Name}}' | grep -x example-main_default || echo MANQUANT)" \
  "réseau docker propre à l'env"
wait_pg "$PROJ" ramet
has_note "$PROJ" ramet r1-classique "la ligne survit à la migration"
assert_eq "coucou-classique" "$(get_upload "$PROJ")" "le témoin uploads survit à la migration"

# ====================================================================== 3 =====
step 3 "ramet new feat-a : clone de main, ports distincts, .env réécrit"
rmt "$PROJ" new feat-a
A=$WT/feat-a
assert_dir "$A"
assert_subvol "$DATA/feat-a"
assert_eq "feat-a" "$(inn "$A" git rev-parse --abbrev-ref HEAD)" "branche du worktree"
wait_pg "$A" ramet
has_note "$A" ramet r1-classique "feat-a hérite des données de main"

PA=$(host_port "$A" feat-a web:80); PM=$(host_port "$PROJ" main web:80)
info "ports web — main:$PM feat-a:$PA"
[ "$PA" != "$PM" ] || die "feat-a et main publient le même port hôte ($PA)"
ok "ports distincts entre envs"
assert_eq "coucou-classique" "$(http_body "http://localhost:$PA/uploads/marker.txt")" "feat-a servi sur son propre port"
assert_eq "example-feat-a" "$(rmt "$A" compose ps --format json | python3 -c 'import json,sys
d=[json.loads(l) for l in sys.stdin if l.strip()]
print(d[0]["Project"] if d else "aucun conteneur")')" "project name compose"
assert_eq "WEB_URL=http://localhost:$PA/uploads/marker.txt" "$(cat "$A/.env" 2>/dev/null)" \
  ".env copié dans feat-a, port réécrit"
assert_eq "coucou-classique" "$(http_body "$(sed -n 's/^WEB_URL=//p' "$A/.env")")" \
  "l'URL du .env de feat-a atteint feat-a"

# ====================================================================== 4 =====
step 4 "ramet sync : le .env de main propagé, ports réécrits"
echo "API_URL=http://localhost:8080/api" >> "$PROJ/.env"
info "variable ajoutée au .env de main"
rmt "$A" sync </dev/null >/dev/null 2>&1 && die "sans terminal ni --yes, sync aurait dû refuser de remplacer"
assert_eq 1 "$(wc -l < "$A/.env" | tr -d ' ')" "refus sans --yes : le .env de feat-a est intact"
rmt "$A" sync --yes
assert_eq "API_URL=http://localhost:$PA/api" "$(grep '^API_URL=' "$A/.env" || true)" \
  "la variable arrive dans feat-a, port réécrit"
rmt "$A" sync | grep -q "already up to date" || die "un second sync devrait n'avoir rien à faire"
ok "second sync : rien à faire"

# ====================================================================== 5 =====
step 5 "isolation des données dans les deux sens"
add_note "$A" ramet r2-feat-a
has_note   "$A" ramet r2-feat-a
lacks_note "$PROJ" ramet r2-feat-a "r2-feat-a absente de main"
add_note "$PROJ" ramet r3-main
has_note   "$PROJ" ramet r3-main
lacks_note "$A" ramet r3-main "r3-main absente de feat-a"
info "main   : $(notes "$PROJ" ramet)"
info "feat-a : $(notes "$A" ramet)"

# ====================================================================== 6 =====
step 6 "checkpoint c1 puis restore : la dernière ligne disparaît"
rmt "$A" checkpoint c1
assert_subvol "$DATA/feat-a@c1"
rmt "$A" log | grep -q c1 || die "c1 absent de ramet log"
ok "c1 listé par ramet log"
add_note "$A" ramet r4-apres-c1
has_note "$A" ramet r4-apres-c1
rmt "$A" restore c1 --yes
wait_pg "$A" ramet
lacks_note "$A" ramet r4-apres-c1 "r4-apres-c1 effacée par le restore"
has_note   "$A" ramet r2-feat-a   "r2-feat-a (antérieure à c1) conservée"
assert_eq "coucou-classique" "$(get_upload "$A")" "uploads cohérent avec pgdata après restore"

# ====================================================================== 7 =====
step 7 "ramet new feat-b --from c1"
rmt "$A" new feat-b --from c1
B_=$WT/feat-b
assert_dir "$B_"
assert_subvol "$DATA/feat-b"
wait_pg "$B_" ramet
has_note   "$B_" ramet r2-feat-a    "feat-b part bien de c1"
lacks_note "$B_" ramet r4-apres-c1  "feat-b n'a pas les données postérieures à c1"
lacks_note "$B_" ramet r3-main      "feat-b n'a pas les données de main"

# ====================================================================== 8 =====
step 8 "compose modifié dans feat-a : nouveau volume nommé pris en compte"
python3 - "$A/compose.yml" <<'PY'
import sys, re
p = sys.argv[1]
s = open(p).read()
s = s.replace("      - uploads:/usr/share/nginx/html/uploads\n",
              "      - uploads:/usr/share/nginx/html/uploads\n      - cache:/var/cache/ramet\n")
s = s.rstrip("\n") + "\n  cache:\n"
open(p, "w").write(s)
PY
grep -q "cache:/var/cache/ramet" "$A/compose.yml" || die "modification du compose ratée"
ok "volume nommé 'cache' ajouté au compose de feat-a"
rmt "$A" compose up -d --wait
assert_dir "$DATA/feat-a/volumes/cache"
rmt "$A" compose exec -T web sh -c 'printf ok > /var/cache/ramet/t && cat /var/cache/ramet/t' | grep -qx ok \
  || die "le nouveau volume n'est pas écrivable"
ok "nouveau volume monté et écrivable"
wait_pg "$A" ramet
has_note "$A" ramet r2-feat-a "les données existantes survivent au reload du compose"

# ====================================================================== 9 =====
step 9 "ls --json cohérent, doctor sans erreur"
rmt "$PROJ" ls --json | python3 -c '
import json,sys
d=json.load(sys.stdin)
envs=d.get("envs",d) if isinstance(d,dict) else d
names={e["name"] for e in envs}
assert names=={"main","feat-a","feat-b"}, "envs listés: %s" % sorted(names)
# main garde ses ports de depart : range null, pas de plage allouee.
# (pas d apostrophe ici : le bloc est dans un python3 -c entre quotes simples)
ports=[tuple(e["ports"]["range"]) for e in envs if e["ports"]["range"]]
assert len(set(ports))==len(ports), "plages de ports en collision: %s" % ports
hosts=[p["host_port"] for e in envs for p in e["published"]]
assert len(set(hosts))==len(hosts), "ports hôtes en collision: %s" % hosts
by={e["name"]:e for e in envs}
assert by["feat-a"]["parent"]=="main", "parent de feat-a: %r" % by["feat-a"]["parent"]
assert by["feat-b"]["parent"]=="feat-a", "parent de feat-b: %r" % by["feat-b"]["parent"]
print("ok")' | grep -qx ok || die "ls --json incohérent"
ok "ls --json : 3 envs, plages de ports disjointes, parents corrects"
rmt "$PROJ" ls | grep -q feat-a || die "ls lisible n'affiche pas feat-a"
ok "ls lisible"
# Un scénario sain ne doit produire aucun avertissement non plus : c'est derrière
# un simple « ! » que se cachait la copie périmée d'env.json dans les checkpoints.
DOC=$(rmt "$PROJ" doctor) || die "doctor signale une erreur :
$DOC"
echo "$DOC" | grep -q '!' && die "doctor signale un avertissement :
$(echo "$DOC" | grep '!')"
ok "doctor sans erreur ni avertissement"

# ===================================================================== 10 =====
step 10 "ramet df puis prune : un worktree supprimé à la main"
rmt "$PROJ" df | sed 's/^/     /'
rmt "$PROJ" df --json | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["image"] is not None, "image de ramet non reconnue"
assert d["quotas"] and d["quotas_consistent"], "quotas btrfs éteints : tailles approximatives"
assert 0 < d["used"] < d["size"], "tailles incohérentes : %s" % d
p=[p for p in d["projects"] if p["name"]=="example"][0]
names=sorted(s["name"] for s in p["subvolumes"])
assert names==["feat-a","feat-a@c1","feat-b","main"], "sous-volumes : %s" % names
assert not any(s["orphan"] for s in p["subvolumes"]), "orphelin inattendu"
assert p["bytes"] > 0, "taille du projet nulle"
main=[s for s in p["subvolumes"] if s["name"]=="main"][0]
assert main["holds_bytes"] > 10**7 and not main["lower_bound"], "main mal mesuré : %s" % main
print("ok")' | grep -qx ok || die "df --json incohérent"
ok "df --json : image reconnue, quotas actifs, main mesuré en entier (pgdata compris)"
rm -rf "$B_"
info "worktree de feat-b supprimé à la main"
rmt "$PROJ" df | grep -q "feat-b .*worktree gone" || die "df ne signale pas feat-b comme orphelin"
ok "df signale feat-b comme orphelin"
rmt "$PROJ" prune </dev/null >/dev/null 2>&1 && die "sans terminal ni --yes, prune aurait dû refuser"
assert_subvol "$DATA/feat-b"
ok "refus sans --yes : rien n'est supprimé"
rmt "$PROJ" prune --yes
assert_nosubvol "$DATA/feat-b"
assert_subvol "$DATA/feat-a"
assert_subvol "$DATA/main"
[ -z "$(docker compose ls -aq --filter name='^example-feat-b$')" ] || die "la stack de feat-b tourne encore"
ok "stack de feat-b arrêtée"
inn "$PROJ" git worktree list | grep -q feat-b && die "git connaît encore le worktree de feat-b" || ok "git a oublié le worktree de feat-b"
rmt "$PROJ" prune | grep -q "nothing to prune" || die "un second prune devrait n'avoir rien à faire"
ok "second prune : rien à faire"

# ===================================================================== 11 =====
step 11 "ramet rm : env, checkpoints et worktree supprimés, main intact"
rmt "$PROJ" rm feat-a --yes
assert_nosubvol "$DATA/feat-a"
assert_nosubvol "$DATA/feat-a@c1"
assert_nodir "$A"
assert_subvol "$DATA/main"
wait_pg "$PROJ" ramet
has_note "$PROJ" ramet r1-classique "main intact"
has_note "$PROJ" ramet r3-main      "main intact"
inn "$PROJ" git worktree list | grep -qE 'feat-(a|b)' && die "worktree résiduel" || ok "aucun worktree résiduel"

# ===================================================================== 12 =====
step 12 "retour au docker compose classique, volumes distincts"
rmt "$PROJ" compose down
inn "$PROJ" docker compose up -d --wait
wait_pg "$PROJ" plain
has_note   "$PROJ" plain r1-classique "la stack classique retrouve ses propres volumes"
lacks_note "$PROJ" plain r3-main      "les données écrites via ramet ne sont pas dans les volumes docker"
assert_eq "coucou-classique" "$(http_body http://localhost:8080/uploads/marker.txt)" "nginx classique de retour sur 8080"

# ===================================================================== 13 =====
step 13 "ramet deinit : le projet est rendu, le dépôt intact"
rmt "$PROJ" deinit --yes
assert_nodir "$DATA"
[ -z "$(inn "$PROJ" git status --porcelain)" ] || die "le dépôt a été modifié"
ok "dépôt intact, aucun fichier ajouté"
wait_pg "$PROJ" plain
has_note "$PROJ" plain r1-classique "la stack classique tourne toujours sur ses volumes"
assert_eq "coucou-classique" "$(http_body http://localhost:8080/uploads/marker.txt)" \
  "nginx classique toujours servi sur 8080"

echo; echo "${G}${B}Scénario complet : OK.${Z}"
finish
