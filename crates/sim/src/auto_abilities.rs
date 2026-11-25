//! Auto-abilities by the names the game gives them (`a_ability`), as equipment records hold them: 0x8000
//! plus the ability's record. Generated from the game's text by the private repo's gen_auto_abilities.py.

/// Sensor
pub const SENSOR: u16 = 0x8000;
/// First Strike
pub const FIRST_STRIKE: u16 = 0x8001;
/// Initiative
pub const INITIATIVE: u16 = 0x8002;
/// Counterattack
pub const COUNTERATTACK: u16 = 0x8003;
/// Evade & Counter
pub const EVADE_AND_COUNTER: u16 = 0x8004;
/// Magic Counter
pub const MAGIC_COUNTER: u16 = 0x8005;
/// Magic Booster
pub const MAGIC_BOOSTER: u16 = 0x8006;
/// Alchemy
pub const ALCHEMY: u16 = 0x8007;
/// Auto-Potion
pub const AUTO_POTION: u16 = 0x8008;
/// Auto-Med
pub const AUTO_MED: u16 = 0x8009;
/// Auto-Phoenix
pub const AUTO_PHOENIX: u16 = 0x800a;
/// Piercing
pub const PIERCING: u16 = 0x800b;
/// Half MP Cost
pub const HALF_MP_COST: u16 = 0x800c;
/// One MP Cost
pub const ONE_MP_COST: u16 = 0x800d;
/// Double Overdrive
pub const DOUBLE_OVERDRIVE: u16 = 0x800e;
/// Triple Overdrive
pub const TRIPLE_OVERDRIVE: u16 = 0x800f;
/// SOS Overdrive
pub const SOS_OVERDRIVE: u16 = 0x8010;
/// Overdrive → AP
pub const OVERDRIVE_TO_AP: u16 = 0x8011;
/// Double AP
pub const DOUBLE_AP: u16 = 0x8012;
/// Triple AP
pub const TRIPLE_AP: u16 = 0x8013;
/// No AP
pub const NO_AP: u16 = 0x8014;
/// Pickpocket
pub const PICKPOCKET: u16 = 0x8015;
/// Master Thief
pub const MASTER_THIEF: u16 = 0x8016;
/// Break HP Limit
pub const BREAK_HP_LIMIT: u16 = 0x8017;
/// Break MP Limit
pub const BREAK_MP_LIMIT: u16 = 0x8018;
/// Break Damage Limit
pub const BREAK_DAMAGE_LIMIT: u16 = 0x8019;
/// Gillionaire
pub const GILLIONAIRE: u16 = 0x801a;
/// HP Stroll
pub const HP_STROLL: u16 = 0x801b;
/// MP Stroll
pub const MP_STROLL: u16 = 0x801c;
/// No Encounters
pub const NO_ENCOUNTERS: u16 = 0x801d;
/// Firestrike
pub const FIRESTRIKE: u16 = 0x801e;
/// Fire Ward
pub const FIRE_WARD: u16 = 0x801f;
/// Fireproof
pub const FIREPROOF: u16 = 0x8020;
/// Fire Eater
pub const FIRE_EATER: u16 = 0x8021;
/// Icestrike
pub const ICESTRIKE: u16 = 0x8022;
/// Ice Ward
pub const ICE_WARD: u16 = 0x8023;
/// Iceproof
pub const ICEPROOF: u16 = 0x8024;
/// Ice Eater
pub const ICE_EATER: u16 = 0x8025;
/// Lightningstrike
pub const LIGHTNINGSTRIKE: u16 = 0x8026;
/// Lightning Ward
pub const LIGHTNING_WARD: u16 = 0x8027;
/// Lightningproof
pub const LIGHTNINGPROOF: u16 = 0x8028;
/// Lightning Eater
pub const LIGHTNING_EATER: u16 = 0x8029;
/// Waterstrike
pub const WATERSTRIKE: u16 = 0x802a;
/// Water Ward
pub const WATER_WARD: u16 = 0x802b;
/// Waterproof
pub const WATERPROOF: u16 = 0x802c;
/// Water Eater
pub const WATER_EATER: u16 = 0x802d;
/// Deathstrike
pub const DEATHSTRIKE: u16 = 0x802e;
/// Deathtouch
pub const DEATHTOUCH: u16 = 0x802f;
/// Deathproof
pub const DEATHPROOF: u16 = 0x8030;
/// Death Ward
pub const DEATH_WARD: u16 = 0x8031;
/// Zombiestrike
pub const ZOMBIESTRIKE: u16 = 0x8032;
/// Zombietouch
pub const ZOMBIETOUCH: u16 = 0x8033;
/// Zombieproof
pub const ZOMBIEPROOF: u16 = 0x8034;
/// Zombie Ward
pub const ZOMBIE_WARD: u16 = 0x8035;
/// Stonestrike
pub const STONESTRIKE: u16 = 0x8036;
/// Stonetouch
pub const STONETOUCH: u16 = 0x8037;
/// Stoneproof
pub const STONEPROOF: u16 = 0x8038;
/// Stone Ward
pub const STONE_WARD: u16 = 0x8039;
/// Poisonstrike
pub const POISONSTRIKE: u16 = 0x803a;
/// Poisontouch
pub const POISONTOUCH: u16 = 0x803b;
/// Poisonproof
pub const POISONPROOF: u16 = 0x803c;
/// Poison Ward
pub const POISON_WARD: u16 = 0x803d;
/// Sleepstrike
pub const SLEEPSTRIKE: u16 = 0x803e;
/// Sleeptouch
pub const SLEEPTOUCH: u16 = 0x803f;
/// Sleepproof
pub const SLEEPPROOF: u16 = 0x8040;
/// Sleep Ward
pub const SLEEP_WARD: u16 = 0x8041;
/// Silencestrike
pub const SILENCESTRIKE: u16 = 0x8042;
/// Silencetouch
pub const SILENCETOUCH: u16 = 0x8043;
/// Silenceproof
pub const SILENCEPROOF: u16 = 0x8044;
/// Silence Ward
pub const SILENCE_WARD: u16 = 0x8045;
/// Darkstrike
pub const DARKSTRIKE: u16 = 0x8046;
/// Darktouch
pub const DARKTOUCH: u16 = 0x8047;
/// Darkproof
pub const DARKPROOF: u16 = 0x8048;
/// Dark Ward
pub const DARK_WARD: u16 = 0x8049;
/// Slowstrike
pub const SLOWSTRIKE: u16 = 0x804a;
/// Slowtouch
pub const SLOWTOUCH: u16 = 0x804b;
/// Slowproof
pub const SLOWPROOF: u16 = 0x804c;
/// Slow Ward
pub const SLOW_WARD: u16 = 0x804d;
/// Confuseproof
pub const CONFUSEPROOF: u16 = 0x804e;
/// Confuse Ward
pub const CONFUSE_WARD: u16 = 0x804f;
/// Berserkproof
pub const BERSERKPROOF: u16 = 0x8050;
/// Berserk Ward
pub const BERSERK_WARD: u16 = 0x8051;
/// Curseproof
pub const CURSEPROOF: u16 = 0x8052;
/// Curse Ward
pub const CURSE_WARD: u16 = 0x8053;
/// Auto-Shell
pub const AUTO_SHELL: u16 = 0x8054;
/// Auto-Protect
pub const AUTO_PROTECT: u16 = 0x8055;
/// Auto-Haste
pub const AUTO_HASTE: u16 = 0x8056;
/// Auto-Regen
pub const AUTO_REGEN: u16 = 0x8057;
/// Auto-Reflect
pub const AUTO_REFLECT: u16 = 0x8058;
/// SOS Shell
pub const SOS_SHELL: u16 = 0x8059;
/// SOS Protect
pub const SOS_PROTECT: u16 = 0x805a;
/// SOS Haste
pub const SOS_HASTE: u16 = 0x805b;
/// SOS Regen
pub const SOS_REGEN: u16 = 0x805c;
/// SOS Reflect
pub const SOS_REFLECT: u16 = 0x805d;
/// SOS NulTide
pub const SOS_NULTIDE: u16 = 0x805e;
/// SOS NulFrost
pub const SOS_NULFROST: u16 = 0x805f;
/// SOS NulShock
pub const SOS_NULSHOCK: u16 = 0x8060;
/// SOS NulBlaze
pub const SOS_NULBLAZE: u16 = 0x8061;
/// Strength +3%
pub const STRENGTH_3_PERCENT: u16 = 0x8062;
/// Strength +5%
pub const STRENGTH_5_PERCENT: u16 = 0x8063;
/// Strength +10%
pub const STRENGTH_10_PERCENT: u16 = 0x8064;
/// Strength +20%
pub const STRENGTH_20_PERCENT: u16 = 0x8065;
/// Magic +3%
pub const MAGIC_3_PERCENT: u16 = 0x8066;
/// Magic +5%
pub const MAGIC_5_PERCENT: u16 = 0x8067;
/// Magic +10%
pub const MAGIC_10_PERCENT: u16 = 0x8068;
/// Magic +20%
pub const MAGIC_20_PERCENT: u16 = 0x8069;
/// Defense +3%
pub const DEFENSE_3_PERCENT: u16 = 0x806a;
/// Defense +5%
pub const DEFENSE_5_PERCENT: u16 = 0x806b;
/// Defense +10%
pub const DEFENSE_10_PERCENT: u16 = 0x806c;
/// Defense +20%
pub const DEFENSE_20_PERCENT: u16 = 0x806d;
/// Magic Def +3%
pub const MAGIC_DEF_3_PERCENT: u16 = 0x806e;
/// Magic Def +5%
pub const MAGIC_DEF_5_PERCENT: u16 = 0x806f;
/// Magic Def +10%
pub const MAGIC_DEF_10_PERCENT: u16 = 0x8070;
/// Magic Def +20%
pub const MAGIC_DEF_20_PERCENT: u16 = 0x8071;
/// HP +5%
pub const HP_5_PERCENT: u16 = 0x8072;
/// HP +10%
pub const HP_10_PERCENT: u16 = 0x8073;
/// HP +20%
pub const HP_20_PERCENT: u16 = 0x8074;
/// HP +30%
pub const HP_30_PERCENT: u16 = 0x8075;
/// MP +5%
pub const MP_5_PERCENT: u16 = 0x8076;
/// MP +10%
pub const MP_10_PERCENT: u16 = 0x8077;
/// MP +20%
pub const MP_20_PERCENT: u16 = 0x8078;
/// MP +30%
pub const MP_30_PERCENT: u16 = 0x8079;
/// Capture
pub const CAPTURE: u16 = 0x807a;
/// Distill Power
pub const DISTILL_POWER: u16 = 0x807c;
/// Distill Mana
pub const DISTILL_MANA: u16 = 0x807d;
/// Distill Speed
pub const DISTILL_SPEED: u16 = 0x807e;
/// Distill Ability
pub const DISTILL_ABILITY: u16 = 0x807f;
/// Ribbon
pub const RIBBON: u16 = 0x8080;
