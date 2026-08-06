mod app;
mod files_pane;
mod ssh_tab;
mod terminal_view;
mod workbench;

fn main() -> anyhow::Result<()> {
    app::run()
}
