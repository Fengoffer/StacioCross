mod app;
mod files_pane;
mod rdp_pane;
mod ssh_tab;
mod terminal_view;
mod tunnel_pane;
mod workbench;

fn main() -> anyhow::Result<()> {
    app::run()
}
