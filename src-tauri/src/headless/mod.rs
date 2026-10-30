//! loov: 服务器无界面模式（Linux 云服务器等没有桌面的环境）。
//!
//! `loov-agent serve` 起常驻进程：核心和桌面版是同一份（[`crate::core_boot`]），登录、隧道、本地路由、
//! 写各家 CLI 的配置都照常，员工在 SSH 里直接用 claude / codex，费用照样记在 Loov。其余子命令
//! （login、status、run…）经本机控制套接字 `~/.loov-agent/loov/ctl.sock`（只有属主能连）和常驻进程
//! 通信：Loov 的状态（凭据、隧道、路由）在常驻进程里，短命的命令行进程不能自己起一份。
//!
//! 桌面版和 `serve` 同一时间只能运行一个（[`crate::core_boot::acquire_core_lock`]）：两者会抢同一个
//! 本地路由端口、互相清理对方的 xray。

#[cfg(unix)]
mod cli;
#[cfg(unix)]
mod ctl;
#[cfg(unix)]
mod logging;
#[cfg(unix)]
mod serve;

/// 子命令名；命令行第一个参数是其中之一时走服务器模式，否则启动桌面版。
pub const COMMANDS: &[&str] = &[
    "serve", "login", "register", "logout", "status", "refresh", "node", "devices", "usage", "restore", "install", "run",
    "safety", "service", "help", "--help", "-h", "version", "--version",
];

/// 是服务器模式的子命令就执行并返回退出码；否则返回 None（继续启动桌面版）。
pub fn main(args: &[String]) -> Option<i32> {
    let cmd = args.get(1)?;
    if !COMMANDS.contains(&cmd.as_str()) {
        return None;
    }
    #[cfg(unix)]
    {
        Some(cli::run(cmd, &args[2..]))
    }
    #[cfg(not(unix))]
    {
        eprintln!("服务器模式只支持 Linux / macOS");
        Some(2)
    }
}
