//! Facts about the machine that are not obtained through external commands.

use std::io;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};

use crate::util::fs::{is_executable, which};

/// Directories holding administration tools, which a regular user's `PATH`
/// may lack: some distributions install `mkfs.btrfs` there.
const SYSTEM_BIN_DIRS: [&str; 2] = ["/usr/sbin", "/sbin"];

/// Size and free space of a filesystem, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Space {
    /// Size of the filesystem. For btrfs on a single device, the size of the
    /// device as btrfs uses it (measured).
    pub size: u64,
    /// Bytes not in use.
    pub free: u64,
    /// Bytes an unprivileged user can still write.
    pub available: u64,
}

impl Space {
    /// Bytes in use.
    pub fn used(&self) -> u64 {
        self.size.saturating_sub(self.free)
    }
}

/// Queries the host operating system.
///
/// Abstracted so that tests control disk space, environment variables,
/// installed programs and privileges without depending on the machine they
/// run on.
pub trait Host {
    /// Where `program` is installed: in `PATH`, or else in a system directory
    /// such as `/usr/sbin`.
    fn find_program(&self, program: &str) -> Option<PathBuf>;

    /// Size and free space of the filesystem holding `path`.
    fn space(&self, path: &Path) -> io::Result<Space>;

    /// Bytes available to an unprivileged user on the filesystem holding `path`.
    fn free_bytes(&self, path: &Path) -> io::Result<u64> {
        self.space(path).map(|space| space.available)
    }

    /// Whether a TCP port can be bound on all interfaces right now.
    fn port_is_free(&self, port: u16) -> bool;

    /// The value of an environment variable, if set and valid Unicode.
    fn var(&self, name: &str) -> Option<String>;

    /// The login name of the current user, for messages.
    fn user_name(&self) -> String;

    /// The effective user id of the process.
    fn effective_uid(&self) -> u32;

    /// Where the kernel's sysfs is mounted: `/sys`.
    fn sysfs(&self) -> PathBuf;
}

/// The real host.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemHost;

impl Host for SystemHost {
    fn find_program(&self, program: &str) -> Option<PathBuf> {
        which(program).or_else(|| {
            SYSTEM_BIN_DIRS
                .iter()
                .map(|dir| Path::new(dir).join(program))
                .find(|candidate| is_executable(candidate))
        })
    }

    fn space(&self, path: &Path) -> io::Result<Space> {
        let stats = rustix::fs::statvfs(path)?;
        let bytes = |blocks: u64| blocks.saturating_mul(stats.f_frsize);
        Ok(Space {
            size: bytes(stats.f_blocks),
            free: bytes(stats.f_bfree),
            available: bytes(stats.f_bavail),
        })
    }

    fn port_is_free(&self, port: u16) -> bool {
        // docker-proxy binds published ports on every interface: binding the
        // wildcard address is what detects them.
        TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).is_ok()
    }

    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn user_name(&self) -> String {
        let uid = rustix::process::getuid().as_raw();
        ["LOGNAME", "USER", "LNAME", "USERNAME"]
            .iter()
            .find_map(|name| self.var(name).filter(|value| !value.is_empty()))
            .or_else(|| {
                let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
                account_name(&passwd, uid).map(str::to_owned)
            })
            .unwrap_or_else(|| uid.to_string())
    }

    fn effective_uid(&self) -> u32 {
        rustix::process::geteuid().as_raw()
    }

    fn sysfs(&self) -> PathBuf {
        PathBuf::from("/sys")
    }
}

/// The account name of `uid` in the content of an `/etc/passwd` file.
fn account_name(passwd: &str, uid: u32) -> Option<&str> {
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        let id = fields.nth(1)?.parse::<u32>().ok()?;
        (id == uid).then_some(name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_account_name_of_a_uid() {
        let passwd =
            "root:x:0:0:root:/root:/bin/sh\n# comment\ndev:x:1000:1000::/home/dev:/bin/sh\n";
        assert_eq!(account_name(passwd, 1000), Some("dev"));
        assert_eq!(account_name(passwd, 0), Some("root"));
        assert_eq!(account_name(passwd, 42), None);
    }

    #[test]
    fn finds_an_installed_program_and_only_that() {
        assert!(SystemHost.find_program("sh").is_some());
        assert_eq!(SystemHost.find_program("ramet-no-such-program"), None);
    }

    #[test]
    fn a_bound_port_is_not_free() {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!SystemHost.port_is_free(port));
        drop(listener);
        assert!(SystemHost.port_is_free(port));
    }

    #[test]
    fn reports_free_space_of_an_existing_path() {
        let space = SystemHost.space(Path::new("/")).unwrap();
        assert!(space.available <= space.free && space.free <= space.size);
        assert!(SystemHost.free_bytes(Path::new("/")).is_ok());
        assert!(
            SystemHost
                .free_bytes(Path::new("/nonexistent/ramet"))
                .is_err()
        );
    }
}
