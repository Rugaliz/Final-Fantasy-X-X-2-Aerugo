//! Treasure of a real installation: records, the equipment they build and the pickup messages (needs
//! `AERUGO_GAME_DIR`).

use aerugo_sim::items::{ItemTables, Treasure, add_item, gil, give, item_count, offset};
use aerugo_vfs::{GAME_DIR_ENV, Language, Vfs, install_from_env};

#[test]
fn treasure_records_and_messages() {
    let Some(inst) = install_from_env() else {
        eprintln!("skipped: set {GAME_DIR_ENV} to a game installation to run this test");
        return;
    };
    let mut vfs = Vfs::new();
    vfs.mount_install(&inst).unwrap();
    let t = ItemTables::load(&vfs, Language::English).expect("item tables");
    assert_eq!(t.item_name(0x2000, false), "Potion");
    assert_eq!(t.key_item_name(0xA001, false), "Flint");
    assert_eq!(t.equipment_name(0x5046, 0, false), "Longsword");

    // The first records: 200 gil, two Potions, a key item; record 4 is equipment.
    assert_eq!(t.treasure(0), Treasure::Gil(200));
    assert_eq!(t.treasure(1), Treasure::Item(0x2000, 2));
    assert_eq!(t.treasure(2), Treasure::KeyItem(0xA000));
    assert_eq!(t.pickup_message(&t.treasure(0), false), "Obtained 200 Gil!");
    assert_eq!(t.pickup_message(&t.treasure(1), false), "Obtained Potion x2!");
    let Treasure::Equipment(e) = t.treasure(4) else { panic!("record 4 is not equipment") };
    // Yuna's weapon with four slots and one ability, named by it, with her model for that name.
    assert_eq!((e[4], e[5], e[0xB]), (1, 0, 4));
    let name = u16::from_le_bytes([e[0], e[1]]);
    assert_eq!(name, 0x500F);
    assert_eq!(u16::from_le_bytes([e[0xC], e[0xD]]), t.equipment_model(name, 1));
    assert!(t.pickup_message(&t.treasure(4), false).starts_with("Obtained "));

    // Shops: the first item shop sells Potions (50 gil, sold for 12) among others; weapon shop equipment is named.
    let stock = t.item_shop(0);
    assert!(stock.contains(&0x2000));
    assert_eq!(t.item_buy_price(0x2000, 100), 50);
    assert_eq!(t.item_sell_price(0x2000), 12);
    let arms = t.weapon_shop(1);
    assert!(!arms.is_empty());
    for e in &arms {
        assert!(u16::from_le_bytes([e[0], e[1]]) >= 0x5000);
        assert!(t.equipment_buy_price(e, 100) > 0);
    }

    // Giving into an image.
    let mut image = vec![0u8; aerugo_sim::save::FFX_IMAGE_SIZE];
    for i in 0..aerugo_sim::items::ITEM_SLOTS {
        image[offset::ITEM_IDS + i * 2] = 0xFF;
    }
    assert!(add_item(&mut image, 0x2000, 1));
    assert!(give(&mut image, &t.treasure(0)));
    assert!(give(&mut image, &t.treasure(1)));
    assert!(give(&mut image, &t.treasure(4)));
    assert_eq!(gil(&image), 200);
    assert_eq!(item_count(&image, 0x2000), 3);
    assert_eq!(image[offset::EQUIPMENT..offset::EQUIPMENT + 2], e[0..2]);
    // Buying and selling.
    assert!(t.buy_item(&mut image, 0x2000, 2, 100));
    assert_eq!(gil(&image), 100);
    assert_eq!(item_count(&image, 0x2000), 5);
    assert!(!t.buy_item(&mut image, 0x2000, 3, 100));
    assert!(t.sell_item(&mut image, 0x2000, 4));
    assert_eq!(gil(&image), 148);
    assert_eq!(item_count(&image, 0x2000), 1);
}
