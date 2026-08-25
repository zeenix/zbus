#[cfg(unix)]
#[test]
fn fd_value() {
    use std::os::fd::AsFd;
    use zvariant::{Basic, Fd, LE};

    #[macro_use]
    mod common {
        include!("common.rs");
    }

    let stdout = std::io::stdout();
    let fd = stdout.as_fd();
    fd_value_test!(LE, DBus, Fd::from(fd), 4, 4, 8);
}
