use aerugo_formats::Game;
use aerugo_sim::field::{Event, Input};
use aerugo_vfs::{Language, Vfs, install_from_env};
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let mut vfs = Vfs::new();
    vfs.mount_install(&install_from_env().unwrap()).unwrap();
    let mut ev = Event::load(&vfs, Game::Ffx, &path, Language::English).unwrap();
    for _ in 0..2 {
        ev.step(Input::default());
    }
    let w = &ev.vm.workers[12];
    println!("w12 level {} queue {:?}", w.level, w.queue);
}
