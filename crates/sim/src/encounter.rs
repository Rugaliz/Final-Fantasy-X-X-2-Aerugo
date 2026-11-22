//! Battles from the field: random encounters as the player walks, and battles scripts start.
//!
//! `btl.bin` lists encounter areas: a header (u32 at 4: index offset, u32 at 8: group data offset), then
//! 14-byte index entries (u16 area id, u16 offset into the group data, u16, 8-byte name such as "bjyt02").
//! An area's data is two bytes (the second: its group count) and its groups, each `[formation count, ?, ?,
//! danger, total weight]` followed by `(formation, weight)` byte pairs; the weight is the pair's high nibble
//! (seeded from the low one when the area is first used).
//!
//! Walking adds the distance to two counters. While the first holds more than 10, 10 comes off it and, once
//! the second (the distance since the last battle, in tens) passes half the danger, a roll on random stream 0
//! against `(steps − danger/2) · 256 / (danger · 4)` starts a battle; stream 1 picks the formation by weight.
//! (After a failed roll, the next 10 compare the chance just used instead of the steps, as the original does.)

/// One encounter group: danger and formations with their weights.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub danger: u8,
    /// (formation, weight now): the weight is the pair's high nibble.
    pub formations: Vec<(u8, u8)>,
    /// Each formation's starting weight (the pair's low nibble).
    pub seeds: Vec<u8>,
}

impl Group {
    pub fn total_weight(&self) -> u32 {
        self.formations.iter().map(|f| u32::from(f.1)).sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Area {
    pub id: u16,
    pub name: String,
    pub groups: Vec<Group>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EncounterTable {
    pub areas: Vec<Area>,
}

impl EncounterTable {
    pub fn parse(d: &[u8]) -> Option<EncounterTable> {
        let u32_at =
            |a: usize| d.get(a..a + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
        let u16_at = |a: usize| d.get(a..a + 2).map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])));
        let (index, groups) = (u32_at(4)?, u32_at(8)?);
        let mut areas = Vec::new();
        for i in 0..groups.checked_sub(index)? / 14 {
            let e = index + i * 14;
            let (id, offset) = (u16_at(e)? as u16, u16_at(e + 2)?);
            let name: String =
                d.get(e + 6..e + 14)?.iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
            let at = groups + offset;
            let count = usize::from(*d.get(at + 1)?);
            let mut list = Vec::with_capacity(count);
            let mut g = at + 2;
            for _ in 0..count {
                let n = usize::from(*d.get(g)?);
                let danger = *d.get(g + 3)?;
                let formations = (0..n)
                    .map(|k| Some((*d.get(g + 5 + k * 2)?, d.get(g + 6 + k * 2)? & 0xF)))
                    .collect::<Option<Vec<_>>>()?;
                let seeds = formations.iter().map(|f| f.1).collect();
                list.push(Group { danger, formations, seeds });
                g += 5 + n * 2;
            }
            areas.push(Area { id, name, groups: list });
        }
        Some(EncounterTable { areas })
    }

    /// The area with this id (ids below 1 never match).
    pub fn area(&self, id: i32) -> Option<usize> {
        if id < 1 {
            return None;
        }
        self.areas.iter().position(|a| i32::from(a.id) == id)
    }

    /// Encounter reset (mode 0): every formation of the area takes its starting weight again.
    pub fn reset_weights(&mut self, area: usize) {
        for g in &mut self.areas[area].groups {
            for (f, &s) in g.formations.iter_mut().zip(&g.seeds) {
                f.1 = s;
            }
        }
    }

    /// A battle label (area id << 16 | formation) as area index, group and formation index.
    pub fn label(&self, label: u32) -> Option<Battle> {
        let area = self.area((label >> 16) as i32)?;
        let formation = (label & 0xFFFF) as u8;
        self.areas[area].groups.iter().enumerate().find_map(|(group, g)| {
            g.formations.iter().position(|f| u32::from(f.0) == label & 0xFFFF).map(|index| Battle {
                area,
                group,
                formation: index,
                formation_id: formation,
                scripted: true,
            })
        })
    }
}

/// A battle about to start: area index, group, which of the group's formations (and its id), and whether a
/// script started it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battle {
    pub area: usize,
    pub group: usize,
    pub formation: usize,
    pub formation_id: u8,
    pub scripted: bool,
}

/// The walking counters.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Counters {
    /// Distance not yet rolled for (10 per roll).
    pub pending: f32,
    /// Distance since the last battle.
    pub total: f32,
}

impl Counters {
    /// Walk `distance` in a group: a battle when a roll hits. `random(stream)` draws from random stream 0 or 1.
    pub fn walk(
        &mut self,
        table: &EncounterTable,
        area: usize,
        group: usize,
        distance: f32,
        mut random: impl FnMut(u8) -> u32,
    ) -> Option<Battle> {
        let g = table.areas.get(area)?.groups.get(group)?;
        let total_weight = g.total_weight();
        if distance == 0.0 || g.danger == 0 || total_weight == 0 {
            return None;
        }
        self.pending += distance;
        self.total += distance;
        let danger = i32::from(g.danger);
        let mut x = (self.total / 10.0) as i32;
        while self.pending > 10.0 {
            let half = danger >> 1;
            if x > half {
                x = (x - half) * 0x100 / (danger << 2);
                if ((random(0) & 0xFF) as i32) < x {
                    let mut pick = (random(1) % total_weight) as i32;
                    for (k, f) in g.formations.iter().enumerate() {
                        pick -= i32::from(f.1);
                        if pick < 0 {
                            *self = Counters::default();
                            return Some(Battle {
                                area,
                                group,
                                formation: k,
                                formation_id: f.0,
                                scripted: false,
                            });
                        }
                    }
                }
            }
            self.pending -= 10.0;
        }
        None
    }
}

/// The random streams battles use (0: encounter rolls, 1: formation picks): the scripts' generator with each
/// stream's own constants.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BattleRandom {
    pub state: [u32; 2],
    /// Values to hand out first (from a recording), per stream.
    pub replay: [std::collections::VecDeque<u32>; 2],
}

impl BattleRandom {
    const MUL: [u32; 2] = [0x7D2B_89DD, 0x6554_2E9B];
    const XOR: [u32; 2] = [0x2813, 0x5FF3];

    pub fn next(&mut self, stream: u8) -> u32 {
        let i = usize::from(stream.min(1));
        if let Some(v) = self.replay[i].pop_front() {
            return v;
        }
        let s = Self::MUL[i].wrapping_mul(self.state[i]) ^ Self::XOR[i];
        let s = (((s as i32) >> 16) as u32).wrapping_add(s << 16);
        self.state[i] = s;
        s & 0x7FFF_FFFF
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> EncounterTable {
        EncounterTable {
            areas: vec![Area {
                id: 0x32,
                name: "cdsp00".into(),
                groups: vec![Group { danger: 60, formations: vec![(0, 1), (1, 3)], seeds: vec![1, 3] }],
            }],
        }
    }

    #[test]
    fn no_battle_before_half_the_danger() {
        let t = table();
        let mut c = Counters::default();
        // 290 units: 29 tens, below half of 60 (30): no roll.
        for _ in 0..290 {
            assert_eq!(c.walk(&t, 0, 0, 1.0, |_| 0), None);
        }
    }

    #[test]
    fn a_hit_picks_a_formation_by_weight_and_resets() {
        let t = table();
        let mut c = Counters::default();
        let mut battle = None;
        // Stream 0 always 0 (hits once the chance is above 0); stream 1 = 2 → the second formation (weights 1, 3).
        for _ in 0..400 {
            if let Some(b) = c.walk(&t, 0, 0, 1.0, |s| if s == 0 { 0 } else { 2 }) {
                battle = Some(b);
                break;
            }
        }
        let b = battle.expect("a battle");
        assert_eq!((b.formation, b.formation_id, b.scripted), (1, 1, false));
        assert_eq!(c, Counters::default());
    }

    #[test]
    fn labels_find_the_formation() {
        let t = table();
        let b = t.label(0x0032_0001).unwrap();
        assert_eq!((b.area, b.group, b.formation, b.scripted), (0, 0, 1, true));
        assert!(t.label(0x0033_0001).is_none());
    }

    #[test]
    fn streams_use_their_own_constants() {
        let mut r = BattleRandom { state: [1, 1], ..Default::default() };
        let a = r.next(0);
        let b = r.next(1);
        assert_ne!(a, b);
        r.replay[0].push_back(7);
        assert_eq!(r.next(0), 7);
    }
}
