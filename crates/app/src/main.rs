mod app;
mod ssh_tab;
mod terminal_view;
mod workbench;

fn main() -> anyhow::Result<()> {
    app::run()
}
