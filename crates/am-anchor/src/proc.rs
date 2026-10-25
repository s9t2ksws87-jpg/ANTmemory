//! 子プロセス（git・powershell）を、画面にウィンドウを出さずに起動する。
//!
//! GUIのclient（Antigravity IDE など）の配下からコンソールのコマンドを起動すると、Windowsは
//! ウィンドウの枠を一瞬作って消す。検索や記録のたびに点滅するので、全ての子プロセスをここから作る。

use std::process::Command;

pub fn silent(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.stdin(std::process::Stdio::null());
    cmd
}
