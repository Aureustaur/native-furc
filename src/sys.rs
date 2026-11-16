//! Small cross-platform process helpers.

use std::process::Command;

/// Creates a `Command` that never flashes a console window on Windows
/// (`CREATE_NO_WINDOW`); identical to `Command::new` elsewhere.
pub fn command<S: AsRef<std::ffi::OsStr>>(program: S) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}
