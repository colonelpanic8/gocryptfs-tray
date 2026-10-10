use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use anyhow::Result;

const MOUNTINFO: &str = "/proc/self/mountinfo";

pub fn is_mounted(path: &Path) -> Result<bool> {
    let text = std::fs::read_to_string(MOUNTINFO)?;
    Ok(mount_points(&text).any(|point| point == path))
}

fn mount_points(mountinfo: &str) -> impl Iterator<Item = PathBuf> + '_ {
    mountinfo
        .lines()
        .filter_map(|line| line.split(' ').nth(4))
        .map(unescape)
}

/// mountinfo escapes space, tab, newline and backslash as `\ooo`.
fn unescape(field: &str) -> PathBuf {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && let Some(octal) = field.get(i + 1..i + 4)
            && let Ok(value) = u8::from_str_radix(octal, 8)
        {
            out.push(value);
            i += 4;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    std::ffi::OsString::from_vec(out).into()
}

/// Calls `on_change` every time the mount table changes. Never returns unless polling fails.
pub fn watch(mut on_change: impl FnMut()) -> Result<()> {
    let file = std::fs::File::open(MOUNTINFO)?;
    loop {
        let mut pollfd = libc::pollfd {
            fd: file.as_raw_fd(),
            events: libc::POLLPRI,
            revents: 0,
        };
        // SAFETY: pollfd points to one valid, initialized pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&mut pollfd, 1, -1) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        // The kernel only re-arms POLLPRI after the file has been read again.
        std::io::Read::read_to_end(&mut &file, &mut Vec::new())?;
        std::io::Seek::rewind(&mut &file)?;
        on_change();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
22 1 259:3 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p3 rw
91 30 0:62 / /run/gocryptfs-tray/private/mnt rw,nosuid,nodev,relatime shared:50 - fuse.gocryptfs /var/lib/syncthing/sync/Private.encrypted rw,user_id=237,group_id=237,default_permissions,allow_other
92 30 0:63 / /home/me/My\\040Vault rw,relatime shared:51 - fuse.gocryptfs /home/me/vault.enc rw
";

    #[test]
    fn finds_mount_points_including_escaped_ones() {
        let points: Vec<PathBuf> = mount_points(SAMPLE).collect();
        assert!(points.contains(&PathBuf::from("/run/gocryptfs-tray/private/mnt")));
        assert!(points.contains(&PathBuf::from("/home/me/My Vault")));
        assert!(!points.contains(&PathBuf::from("/run/gocryptfs-tray/private")));
    }
}
