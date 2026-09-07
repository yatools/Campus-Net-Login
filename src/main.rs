#![windows_subsystem = "windows"]

fn main() {
    if let Err(message) = campus_net_auto_login::ui::run() {
        campus_net_auto_login::ui::show_fatal_error(&message);
    }
}
