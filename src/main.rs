//! 二进制入口。
//!
//! 只做一件事：把退出码交给 `cli::run`。所有子命令（`daemon` / `trigger` /
//! `doctor` …）都由它分派——同一个可执行文件既当 daemon 又当 CLI，
//! 这样安装时只产出一个二进制、一条软链。

use std::process::ExitCode;

fn main() -> ExitCode {
    semisnip::cli::run()
}
