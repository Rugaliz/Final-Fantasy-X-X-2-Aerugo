//! The game's random number streams: 68 generators, each `state = mul·state ^ xor`, then the state's halves
//! swapped with the high half shifted arithmetically; the value is the state's low 31 bits. A start seeds every
//! stream from one master generator, itself seeded from a value the original takes from the clock (so any seed
//! is one the original could have drawn).

const MUL: [u32; 68] = [
    0x7D2B89DD, 0x65542E9B, 0x0EBB6BD7, 0x35256BA6, 0x509D2660, 0x5D2D3695, 0x5B147842, 0x1E80017C,
    0x67B5316A, 0x17BB3F01, 0x4C302A37, 0x01402228, 0x464279A5, 0x2FDB6F1D, 0x43642F54, 0x5F580F83,
    0x38C90CF4, 0xCA6DD8A7, 0xBE8E2B3E, 0x87FCB865, 0xEBCECF08, 0xDAB84AB0, 0x85D359D2, 0xDF31B81B,
    0xFFADD5BC, 0xF85288BF, 0x97551D91, 0x523BF81E, 0x35F042C4, 0xB7E893E2, 0x9C14CFB9, 0xB33C0C51,
    0x6293079A, 0x1779156F, 0xCED4B944, 0x6D883085, 0x3863428B, 0x4A65C695, 0x9B9F0A9D, 0x82546D1D,
    0xD75C27F8, 0x3BD78424, 0x5FD4DF1D, 0xDD1C81CF, 0x4C9A8652, 0x7708A851, 0xF9A41118, 0x70251ABA,
    0x1C62248C, 0x12536864, 0x4CEF37DA, 0x09AFA9E8, 0xF3418857, 0xBC4A5CF2, 0xB42A28BD, 0x58776FD1,
    0xA306320E, 0x53E6B0EE, 0x47E4CBEE, 0xA45DEE02, 0x2614B577, 0x22ABB5AF, 0x456A386D, 0xAA5EE91E,
    0x542AA806, 0xA451100A, 0xEF0398B0, 0x2A14C998,
];
const XOR: [u16; 68] = [
    0x2813, 0x5FF3, 0x2BA9, 0xDE78, 0xB475, 0xC2A2, 0x69C5, 0x04E9, 0xAC84, 0xDCF5, 0x7921, 0xB61A, 0xFB8D,
    0xB409, 0xE307, 0x3322, 0x4C28, 0xBA54, 0x52AB, 0x3F77, 0x023E, 0x48E2, 0xEC4F, 0xA44A, 0x9E54, 0x3551,
    0x1F71, 0x6319, 0x249D, 0xBF35, 0x5A75, 0x7FDF, 0x73A5, 0xABED, 0x40EF, 0x3AF2, 0xAAF7, 0xC892, 0x6F45,
    0x9918, 0x9C95, 0x807D, 0xA1B8, 0x04F3, 0x3C4C, 0x836D, 0x9145, 0xE319, 0x3F88, 0xE911, 0xD19F, 0x2D08,
    0x92D0, 0x47FB, 0xE9B3, 0xC131, 0x598A, 0x5E94, 0xF543, 0xDBB1, 0xD816, 0x2599, 0xE116, 0x1D9C, 0xDC95,
    0xC038, 0xA2C7, 0x8E6A,
];

/// All streams' states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RngStreams {
    pub state: [u32; 68],
}

impl Default for RngStreams {
    fn default() -> Self {
        RngStreams::seeded(1, 0)
    }
}

impl RngStreams {
    /// The seeding at a save's start: the master generator from `clock` (the original's clock-byte value, 1–256)
    /// times `n + 1`, one value dropped, then one value per stream.
    pub fn seeded(clock: u32, n: u32) -> RngStreams {
        let seed = clock.wrapping_mul(n.wrapping_add(1));
        let mut master = seed.wrapping_mul(0x420C_56D7).wrapping_add(0x2E0A);
        let mut draw = || {
            let v = master.wrapping_mul(0x5D58_8B65).wrapping_add(0x3C35);
            master = (((v as i32) >> 16) as u32).wrapping_add(v << 16);
            master & 0x7FFF_FFFF
        };
        draw();
        RngStreams { state: std::array::from_fn(|_| draw()) }
    }

    /// The next value of stream `i`.
    pub fn next(&mut self, i: usize) -> u32 {
        let s = MUL[i].wrapping_mul(self.state[i]) ^ u32::from(XOR[i]);
        let s = (((s as i32) >> 16) as u32).wrapping_add(s << 16);
        self.state[i] = s;
        s & 0x7FFF_FFFF
    }
}
