use my_editor::{Result, error::install_panic_hook, runtime::Runtime, terminal::TerminalSession};
use std::path::PathBuf;

fn main() {
    cap_malloc_arenas();
    install_panic_hook();

    if let Err(error) = run() {
        eprintln!("my_editor: {error}");
        std::process::exit(1);
    }
}

/// glibc の malloc arena をエディタ本体と子プロセス双方で1個に固定し、VSZ 肥大を防ぐ。
/// arena はスレッドごとに増え、その上限は既定で `8 × コア数` なので、コアの多い環境ほど
/// 実使用(RSS)は据え置きのまま仮想メモリだけが膨らむ。エディタは実質シングルスレッドで
/// arena 分割の利点が無く、rust-analyzer は多コア環境で数GB規模の VSZ を予約するため、
/// ここで抑える。プロセス内は `mallopt` で、spawn する rust-analyzer / git / シェルへは
/// 環境変数 `MALLOC_ARENA_MAX` の継承で効かせる。
fn cap_malloc_arenas() {
    #[cfg(target_env = "gnu")]
    // SAFETY: 起動直後・シングルスレッドで、mallopt / environ への並行アクセスはない。
    unsafe {
        nix::libc::mallopt(nix::libc::M_ARENA_MAX, 1);
        std::env::set_var("MALLOC_ARENA_MAX", "1");
    }
}

fn run() -> Result<()> {
    let tokio = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let terminal = TerminalSession::enter()?;
    let cwd = std::env::current_dir()?;
    let paths = std::env::args()
        .skip(1)
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        })
        .collect();
    let mut runtime = Runtime::new(terminal, paths);
    tokio.block_on(runtime.run())
}
