mod app;
mod terminal_view;
mod workbench;

fn main() -> anyhow::Result<()> {
    app::run()
}
