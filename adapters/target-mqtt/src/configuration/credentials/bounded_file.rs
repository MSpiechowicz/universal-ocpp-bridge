use std::{
    fs::{File, OpenOptions},
    io::Read as _,
    path::Path,
};

pub(super) fn read_bounded_file(path: &Path, maximum: u64, private: bool) -> Result<Vec<u8>, ()> {
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(path)
        .map_err(|_| ())?
        .file_type()
        .is_symlink()
    {
        return Err(());
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| ())?;
    validate_handle(&file, maximum, private)?;

    let read_limit = maximum.checked_add(1).ok_or(())?;
    let mut bytes = Vec::new();
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if u64::try_from(bytes.len()).map_err(|_| ())? > maximum {
        return Err(());
    }
    Ok(bytes)
}

fn validate_handle(file: &File, maximum: u64, private: bool) -> Result<(), ()> {
    let metadata = file.metadata().map_err(|_| ())?;
    if !metadata.file_type().is_file() || metadata.len() > maximum {
        return Err(());
    }
    #[cfg(not(unix))]
    let _ = private;
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::read_bounded_file;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "uob-mqtt-bounded-file-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self, name: &str) -> std::path::PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove test directory");
        }
    }

    #[test]
    fn exact_limit_is_accepted_and_one_extra_byte_is_rejected() {
        let directory = TestDirectory::new();
        let exact = directory.path("exact");
        let overflow = directory.path("overflow");
        fs::write(&exact, b"1234").expect("write exact file");
        fs::write(&overflow, b"12345").expect("write overflow file");

        assert_eq!(read_bounded_file(&exact, 4, false), Ok(b"1234".to_vec()));
        assert_eq!(read_bounded_file(&overflow, 4, false), Err(()));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_non_regular_files_and_public_private_material() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let directory = TestDirectory::new();
        let private = directory.path("private");
        let link = directory.path("link");
        fs::write(&private, b"secret").expect("write private file");
        fs::set_permissions(&private, fs::Permissions::from_mode(0o644)).expect("set public mode");
        symlink(&private, &link).expect("create symlink");

        assert_eq!(read_bounded_file(&private, 64, true), Err(()));
        assert_eq!(read_bounded_file(&link, 64, false), Err(()));
        assert_eq!(read_bounded_file(&directory.0, 64, false), Err(()));

        fs::set_permissions(&private, fs::Permissions::from_mode(0o600)).expect("set private mode");
        assert_eq!(
            read_bounded_file(&private, 64, true),
            Ok(b"secret".to_vec())
        );
    }
}
