//! Block elements, sextants, octants, and legacy diagonal block polygons.

// Geometry from WezTerm 577474d89ee61aef4a48145cdec82a638d874751, MIT; see LICENSE-WEZTERM.
use super::*;

// Lookup table from sextant Unicode range 0x1fb00..=0x1fb3b to sextant pattern:
// `pattern` is a byte whose bits corresponds to elements on a 2 by 3 grid.
// The position of a sextant for a bit position (0-indexed) is as follows:
// ╭───┬───╮
// │ 0 │ 1 │
// ├───┼───┤
// │ 2 │ 3 │
// ├───┼───┤
// │ 4 │ 5 │
// ╰───┴───╯
const SEXTANT_PATTERNS: [u8; 60] = [
    0b000001, // [🬀] BLOCK SEXTANT-1
    0b000010, // [🬁] BLOCK SEXTANT-2
    0b000011, // [🬂] BLOCK SEXTANT-12
    0b000100, // [🬃] BLOCK SEXTANT-3
    0b000101, // [🬄] BLOCK SEXTANT-13
    0b000110, // [🬅] BLOCK SEXTANT-23
    0b000111, // [🬆] BLOCK SEXTANT-123
    0b001000, // [🬇] BLOCK SEXTANT-4
    0b001001, // [🬈] BLOCK SEXTANT-14
    0b001010, // [🬉] BLOCK SEXTANT-24
    0b001011, // [🬊] BLOCK SEXTANT-124
    0b001100, // [🬋] BLOCK SEXTANT-34
    0b001101, // [🬌] BLOCK SEXTANT-134
    0b001110, // [🬍] BLOCK SEXTANT-234
    0b001111, // [🬎] BLOCK SEXTANT-1234
    0b010000, // [🬏] BLOCK SEXTANT-5
    0b010001, // [🬐] BLOCK SEXTANT-15
    0b010010, // [🬑] BLOCK SEXTANT-25
    0b010011, // [🬒] BLOCK SEXTANT-125
    0b010100, // [🬓] BLOCK SEXTANT-35
    0b010110, // [🬔] BLOCK SEXTANT-235
    0b010111, // [🬕] BLOCK SEXTANT-1235
    0b011000, // [🬖] BLOCK SEXTANT-45
    0b011001, // [🬗] BLOCK SEXTANT-145
    0b011010, // [🬘] BLOCK SEXTANT-245
    0b011011, // [🬙] BLOCK SEXTANT-1245
    0b011100, // [🬚] BLOCK SEXTANT-345
    0b011101, // [🬛] BLOCK SEXTANT-1345
    0b011110, // [🬜] BLOCK SEXTANT-2345
    0b011111, // [🬝] BLOCK SEXTANT-12345
    0b100000, // [🬞] BLOCK SEXTANT-6
    0b100001, // [🬟] BLOCK SEXTANT-16
    0b100010, // [🬠] BLOCK SEXTANT-26
    0b100011, // [🬡] BLOCK SEXTANT-126
    0b100100, // [🬢] BLOCK SEXTANT-36
    0b100101, // [🬣] BLOCK SEXTANT-136
    0b100110, // [🬤] BLOCK SEXTANT-236
    0b100111, // [🬥] BLOCK SEXTANT-1236
    0b101000, // [🬦] BLOCK SEXTANT-46
    0b101001, // [🬧] BLOCK SEXTANT-146
    0b101011, // [🬨] BLOCK SEXTANT-1246
    0b101100, // [🬩] BLOCK SEXTANT-346
    0b101101, // [🬪] BLOCK SEXTANT-1346
    0b101110, // [🬫] BLOCK SEXTANT-2346
    0b101111, // [🬬] BLOCK SEXTANT-12346
    0b110000, // [🬭] BLOCK SEXTANT-56
    0b110001, // [🬮] BLOCK SEXTANT-156
    0b110010, // [🬯] BLOCK SEXTANT-256
    0b110011, // [🬰] BLOCK SEXTANT-1256
    0b110100, // [🬱] BLOCK SEXTANT-356
    0b110101, // [🬲] BLOCK SEXTANT-1356
    0b110110, // [🬳] BLOCK SEXTANT-2356
    0b110111, // [🬴] BLOCK SEXTANT-12356
    0b111000, // [🬵] BLOCK SEXTANT-456
    0b111001, // [🬶] BLOCK SEXTANT-1456
    0b111010, // [🬷] BLOCK SEXTANT-2456
    0b111011, // [🬸] BLOCK SEXTANT-12456
    0b111100, // [🬹] BLOCK SEXTANT-3456
    0b111101, // [🬺] BLOCK SEXTANT-13456
    0b111110, // [🬻] BLOCK SEXTANT-23456
];

// Lookup table from octant Unicode range 0x1cd00..=0x1cde5 to octant pattern:
// `pattern` is a byte whose bits corresponds to elements on a 2 by 4 grid.
// The position of a octant for a bit position (0-indexed) is as follows:
// ╭───┬───╮
// │ 0 │ 1 │
// ├───┼───┤
// │ 2 │ 3 │
// ├───┼───┤
// │ 4 │ 5 │
// ├───┼───┤
// │ 6 │ 7 │
// ╰───┴───╯
const OCTANT_PATTERNS: [u8; 230] = [
    0b00000100, // 1CD00;BLOCK OCTANT-3
    0b00000110, // 1CD01;BLOCK OCTANT-23
    0b00000111, // 1CD02;BLOCK OCTANT-123
    0b00001000, // 1CD03;BLOCK OCTANT-4
    0b00001001, // 1CD04;BLOCK OCTANT-14
    0b00001011, // 1CD05;BLOCK OCTANT-124
    0b00001100, // 1CD06;BLOCK OCTANT-34
    0b00001101, // 1CD07;BLOCK OCTANT-134
    0b00001110, // 1CD08;BLOCK OCTANT-234
    0b00010000, // 1CD09;BLOCK OCTANT-5
    0b00010001, // 1CD0A;BLOCK OCTANT-15
    0b00010010, // 1CD0B;BLOCK OCTANT-25
    0b00010011, // 1CD0C;BLOCK OCTANT-125
    0b00010101, // 1CD0D;BLOCK OCTANT-135
    0b00010110, // 1CD0E;BLOCK OCTANT-235
    0b00010111, // 1CD0F;BLOCK OCTANT-1235
    0b00011000, // 1CD10;BLOCK OCTANT-45
    0b00011001, // 1CD11;BLOCK OCTANT-145
    0b00011010, // 1CD12;BLOCK OCTANT-245
    0b00011011, // 1CD13;BLOCK OCTANT-1245
    0b00011100, // 1CD14;BLOCK OCTANT-345
    0b00011101, // 1CD15;BLOCK OCTANT-1345
    0b00011110, // 1CD16;BLOCK OCTANT-2345
    0b00011111, // 1CD17;BLOCK OCTANT-12345
    0b00100000, // 1CD18;BLOCK OCTANT-6
    0b00100001, // 1CD19;BLOCK OCTANT-16
    0b00100010, // 1CD1A;BLOCK OCTANT-26
    0b00100011, // 1CD1B;BLOCK OCTANT-126
    0b00100100, // 1CD1C;BLOCK OCTANT-36
    0b00100101, // 1CD1D;BLOCK OCTANT-136
    0b00100110, // 1CD1E;BLOCK OCTANT-236
    0b00100111, // 1CD1F;BLOCK OCTANT-1236
    0b00101001, // 1CD20;BLOCK OCTANT-146
    0b00101010, // 1CD21;BLOCK OCTANT-246
    0b00101011, // 1CD22;BLOCK OCTANT-1246
    0b00101100, // 1CD23;BLOCK OCTANT-346
    0b00101101, // 1CD24;BLOCK OCTANT-1346
    0b00101110, // 1CD25;BLOCK OCTANT-2346
    0b00101111, // 1CD26;BLOCK OCTANT-12346
    0b00110000, // 1CD27;BLOCK OCTANT-56
    0b00110001, // 1CD28;BLOCK OCTANT-156
    0b00110010, // 1CD29;BLOCK OCTANT-256
    0b00110011, // 1CD2A;BLOCK OCTANT-1256
    0b00110100, // 1CD2B;BLOCK OCTANT-356
    0b00110101, // 1CD2C;BLOCK OCTANT-1356
    0b00110110, // 1CD2D;BLOCK OCTANT-2356
    0b00110111, // 1CD2E;BLOCK OCTANT-12356
    0b00111000, // 1CD2F;BLOCK OCTANT-456
    0b00111001, // 1CD30;BLOCK OCTANT-1456
    0b00111010, // 1CD31;BLOCK OCTANT-2456
    0b00111011, // 1CD32;BLOCK OCTANT-12456
    0b00111100, // 1CD33;BLOCK OCTANT-3456
    0b00111101, // 1CD34;BLOCK OCTANT-13456
    0b00111110, // 1CD35;BLOCK OCTANT-23456
    0b01000001, // 1CD36;BLOCK OCTANT-17
    0b01000010, // 1CD37;BLOCK OCTANT-27
    0b01000011, // 1CD38;BLOCK OCTANT-127
    0b01000100, // 1CD39;BLOCK OCTANT-37
    0b01000101, // 1CD3A;BLOCK OCTANT-137
    0b01000110, // 1CD3B;BLOCK OCTANT-237
    0b01000111, // 1CD3C;BLOCK OCTANT-1237
    0b01001000, // 1CD3D;BLOCK OCTANT-47
    0b01001001, // 1CD3E;BLOCK OCTANT-147
    0b01001010, // 1CD3F;BLOCK OCTANT-247
    0b01001011, // 1CD40;BLOCK OCTANT-1247
    0b01001100, // 1CD41;BLOCK OCTANT-347
    0b01001101, // 1CD42;BLOCK OCTANT-1347
    0b01001110, // 1CD43;BLOCK OCTANT-2347
    0b01001111, // 1CD44;BLOCK OCTANT-12347
    0b01010001, // 1CD45;BLOCK OCTANT-157
    0b01010010, // 1CD46;BLOCK OCTANT-257
    0b01010011, // 1CD47;BLOCK OCTANT-1257
    0b01010100, // 1CD48;BLOCK OCTANT-357
    0b01010110, // 1CD49;BLOCK OCTANT-2357
    0b01010111, // 1CD4A;BLOCK OCTANT-12357
    0b01011000, // 1CD4B;BLOCK OCTANT-457
    0b01011001, // 1CD4C;BLOCK OCTANT-1457
    0b01011011, // 1CD4D;BLOCK OCTANT-12457
    0b01011100, // 1CD4E;BLOCK OCTANT-3457
    0b01011101, // 1CD4F;BLOCK OCTANT-13457
    0b01011110, // 1CD50;BLOCK OCTANT-23457
    0b01100000, // 1CD51;BLOCK OCTANT-67
    0b01100001, // 1CD52;BLOCK OCTANT-167
    0b01100010, // 1CD53;BLOCK OCTANT-267
    0b01100011, // 1CD54;BLOCK OCTANT-1267
    0b01100100, // 1CD55;BLOCK OCTANT-367
    0b01100101, // 1CD56;BLOCK OCTANT-1367
    0b01100110, // 1CD57;BLOCK OCTANT-2367
    0b01100111, // 1CD58;BLOCK OCTANT-12367
    0b01101000, // 1CD59;BLOCK OCTANT-467
    0b01101001, // 1CD5A;BLOCK OCTANT-1467
    0b01101010, // 1CD5B;BLOCK OCTANT-2467
    0b01101011, // 1CD5C;BLOCK OCTANT-12467
    0b01101100, // 1CD5D;BLOCK OCTANT-3467
    0b01101101, // 1CD5E;BLOCK OCTANT-13467
    0b01101110, // 1CD5F;BLOCK OCTANT-23467
    0b01101111, // 1CD60;BLOCK OCTANT-123467
    0b01110000, // 1CD61;BLOCK OCTANT-567
    0b01110001, // 1CD62;BLOCK OCTANT-1567
    0b01110010, // 1CD63;BLOCK OCTANT-2567
    0b01110011, // 1CD64;BLOCK OCTANT-12567
    0b01110100, // 1CD65;BLOCK OCTANT-3567
    0b01110101, // 1CD66;BLOCK OCTANT-13567
    0b01110110, // 1CD67;BLOCK OCTANT-23567
    0b01110111, // 1CD68;BLOCK OCTANT-123567
    0b01111000, // 1CD69;BLOCK OCTANT-4567
    0b01111001, // 1CD6A;BLOCK OCTANT-14567
    0b01111010, // 1CD6B;BLOCK OCTANT-24567
    0b01111011, // 1CD6C;BLOCK OCTANT-124567
    0b01111100, // 1CD6D;BLOCK OCTANT-34567
    0b01111101, // 1CD6E;BLOCK OCTANT-134567
    0b01111110, // 1CD6F;BLOCK OCTANT-234567
    0b01111111, // 1CD70;BLOCK OCTANT-1234567
    0b10000001, // 1CD71;BLOCK OCTANT-18
    0b10000010, // 1CD72;BLOCK OCTANT-28
    0b10000011, // 1CD73;BLOCK OCTANT-128
    0b10000100, // 1CD74;BLOCK OCTANT-38
    0b10000101, // 1CD75;BLOCK OCTANT-138
    0b10000110, // 1CD76;BLOCK OCTANT-238
    0b10000111, // 1CD77;BLOCK OCTANT-1238
    0b10001000, // 1CD78;BLOCK OCTANT-48
    0b10001001, // 1CD79;BLOCK OCTANT-148
    0b10001010, // 1CD7A;BLOCK OCTANT-248
    0b10001011, // 1CD7B;BLOCK OCTANT-1248
    0b10001100, // 1CD7C;BLOCK OCTANT-348
    0b10001101, // 1CD7D;BLOCK OCTANT-1348
    0b10001110, // 1CD7E;BLOCK OCTANT-2348
    0b10001111, // 1CD7F;BLOCK OCTANT-12348
    0b10010000, // 1CD80;BLOCK OCTANT-58
    0b10010001, // 1CD81;BLOCK OCTANT-158
    0b10010010, // 1CD82;BLOCK OCTANT-258
    0b10010011, // 1CD83;BLOCK OCTANT-1258
    0b10010100, // 1CD84;BLOCK OCTANT-358
    0b10010101, // 1CD85;BLOCK OCTANT-1358
    0b10010110, // 1CD86;BLOCK OCTANT-2358
    0b10010111, // 1CD87;BLOCK OCTANT-12358
    0b10011000, // 1CD88;BLOCK OCTANT-458
    0b10011001, // 1CD89;BLOCK OCTANT-1458
    0b10011010, // 1CD8A;BLOCK OCTANT-2458
    0b10011011, // 1CD8B;BLOCK OCTANT-12458
    0b10011100, // 1CD8C;BLOCK OCTANT-3458
    0b10011101, // 1CD8D;BLOCK OCTANT-13458
    0b10011110, // 1CD8E;BLOCK OCTANT-23458
    0b10011111, // 1CD8F;BLOCK OCTANT-123458
    0b10100001, // 1CD90;BLOCK OCTANT-168
    0b10100010, // 1CD91;BLOCK OCTANT-268
    0b10100011, // 1CD92;BLOCK OCTANT-1268
    0b10100100, // 1CD93;BLOCK OCTANT-368
    0b10100110, // 1CD94;BLOCK OCTANT-2368
    0b10100111, // 1CD95;BLOCK OCTANT-12368
    0b10101000, // 1CD96;BLOCK OCTANT-468
    0b10101001, // 1CD97;BLOCK OCTANT-1468
    0b10101011, // 1CD98;BLOCK OCTANT-12468
    0b10101100, // 1CD99;BLOCK OCTANT-3468
    0b10101101, // 1CD9A;BLOCK OCTANT-13468
    0b10101110, // 1CD9B;BLOCK OCTANT-23468
    0b10110000, // 1CD9C;BLOCK OCTANT-568
    0b10110001, // 1CD9D;BLOCK OCTANT-1568
    0b10110010, // 1CD9E;BLOCK OCTANT-2568
    0b10110011, // 1CD9F;BLOCK OCTANT-12568
    0b10110100, // 1CDA0;BLOCK OCTANT-3568
    0b10110101, // 1CDA1;BLOCK OCTANT-13568
    0b10110110, // 1CDA2;BLOCK OCTANT-23568
    0b10110111, // 1CDA3;BLOCK OCTANT-123568
    0b10111000, // 1CDA4;BLOCK OCTANT-4568
    0b10111001, // 1CDA5;BLOCK OCTANT-14568
    0b10111010, // 1CDA6;BLOCK OCTANT-24568
    0b10111011, // 1CDA7;BLOCK OCTANT-124568
    0b10111100, // 1CDA8;BLOCK OCTANT-34568
    0b10111101, // 1CDA9;BLOCK OCTANT-134568
    0b10111110, // 1CDAA;BLOCK OCTANT-234568
    0b10111111, // 1CDAB;BLOCK OCTANT-1234568
    0b11000001, // 1CDAC;BLOCK OCTANT-178
    0b11000010, // 1CDAD;BLOCK OCTANT-278
    0b11000011, // 1CDAE;BLOCK OCTANT-1278
    0b11000100, // 1CDAF;BLOCK OCTANT-378
    0b11000101, // 1CDB0;BLOCK OCTANT-1378
    0b11000110, // 1CDB1;BLOCK OCTANT-2378
    0b11000111, // 1CDB2;BLOCK OCTANT-12378
    0b11001000, // 1CDB3;BLOCK OCTANT-478
    0b11001001, // 1CDB4;BLOCK OCTANT-1478
    0b11001010, // 1CDB5;BLOCK OCTANT-2478
    0b11001011, // 1CDB6;BLOCK OCTANT-12478
    0b11001100, // 1CDB7;BLOCK OCTANT-3478
    0b11001101, // 1CDB8;BLOCK OCTANT-13478
    0b11001110, // 1CDB9;BLOCK OCTANT-23478
    0b11001111, // 1CDBA;BLOCK OCTANT-123478
    0b11010000, // 1CDBB;BLOCK OCTANT-578
    0b11010001, // 1CDBC;BLOCK OCTANT-1578
    0b11010010, // 1CDBD;BLOCK OCTANT-2578
    0b11010011, // 1CDBE;BLOCK OCTANT-12578
    0b11010100, // 1CDBF;BLOCK OCTANT-3578
    0b11010101, // 1CDC0;BLOCK OCTANT-13578
    0b11010110, // 1CDC1;BLOCK OCTANT-23578
    0b11010111, // 1CDC2;BLOCK OCTANT-123578
    0b11011000, // 1CDC3;BLOCK OCTANT-4578
    0b11011001, // 1CDC4;BLOCK OCTANT-14578
    0b11011010, // 1CDC5;BLOCK OCTANT-24578
    0b11011011, // 1CDC6;BLOCK OCTANT-124578
    0b11011100, // 1CDC7;BLOCK OCTANT-34578
    0b11011101, // 1CDC8;BLOCK OCTANT-134578
    0b11011110, // 1CDC9;BLOCK OCTANT-234578
    0b11011111, // 1CDCA;BLOCK OCTANT-1234578
    0b11100000, // 1CDCB;BLOCK OCTANT-678
    0b11100001, // 1CDCC;BLOCK OCTANT-1678
    0b11100010, // 1CDCD;BLOCK OCTANT-2678
    0b11100011, // 1CDCE;BLOCK OCTANT-12678
    0b11100100, // 1CDCF;BLOCK OCTANT-3678
    0b11100101, // 1CDD0;BLOCK OCTANT-13678
    0b11100110, // 1CDD1;BLOCK OCTANT-23678
    0b11100111, // 1CDD2;BLOCK OCTANT-123678
    0b11101000, // 1CDD3;BLOCK OCTANT-4678
    0b11101001, // 1CDD4;BLOCK OCTANT-14678
    0b11101010, // 1CDD5;BLOCK OCTANT-24678
    0b11101011, // 1CDD6;BLOCK OCTANT-124678
    0b11101100, // 1CDD7;BLOCK OCTANT-34678
    0b11101101, // 1CDD8;BLOCK OCTANT-134678
    0b11101110, // 1CDD9;BLOCK OCTANT-234678
    0b11101111, // 1CDDA;BLOCK OCTANT-1234678
    0b11110001, // 1CDDB;BLOCK OCTANT-15678
    0b11110010, // 1CDDC;BLOCK OCTANT-25678
    0b11110011, // 1CDDD;BLOCK OCTANT-125678
    0b11110100, // 1CDDE;BLOCK OCTANT-35678
    0b11110110, // 1CDDF;BLOCK OCTANT-235678
    0b11110111, // 1CDE0;BLOCK OCTANT-1235678
    0b11111000, // 1CDE1;BLOCK OCTANT-45678
    0b11111001, // 1CDE2;BLOCK OCTANT-145678
    0b11111011, // 1CDE3;BLOCK OCTANT-1245678
    0b11111101, // 1CDE4;BLOCK OCTANT-1345678
    0b11111110, // 1CDE5;BLOCK OCTANT-2345678
];

impl BlockKey {
    /// Map the codepoints owned by this glyph family without changing their geometry.
    pub(super) fn from_blocks_legacy(codepoint: u32) -> Option<Self> {
        Some(match codepoint {
            // [▀] UPPER HALF BLOCK
            0x2580 => Self::Blocks(&[Block::UpperBlock(4)]),
            // [▁] LOWER 1 EIGHTH BLOCK
            0x2581 => Self::Blocks(&[Block::LowerBlock(1)]),
            // [▂] LOWER 2 EIGHTHS BLOCK
            0x2582 => Self::Blocks(&[Block::LowerBlock(2)]),
            // [▃] LOWER 3 EIGHTHS BLOCK
            0x2583 => Self::Blocks(&[Block::LowerBlock(3)]),
            // [▄] LOWER 4 EIGHTHS BLOCK
            0x2584 => Self::Blocks(&[Block::LowerBlock(4)]),
            // [▅] LOWER 5 EIGHTHS BLOCK
            0x2585 => Self::Blocks(&[Block::LowerBlock(5)]),
            // [▆] LOWER 6 EIGHTHS BLOCK
            0x2586 => Self::Blocks(&[Block::LowerBlock(6)]),
            // [▇] LOWER 7 EIGHTHS BLOCK
            0x2587 => Self::Blocks(&[Block::LowerBlock(7)]),
            // [█] FULL BLOCK
            0x2588 => Self::Blocks(&[Block::Custom(0, 8, 0, 8, BlockAlpha::Full)]),
            // [▉] LEFT 7 EIGHTHS BLOCK
            0x2589 => Self::Blocks(&[Block::LeftBlock(7)]),
            // [▊] LEFT 6 EIGHTHS BLOCK
            0x258a => Self::Blocks(&[Block::LeftBlock(6)]),
            // [▋] LEFT 5 EIGHTHS BLOCK
            0x258b => Self::Blocks(&[Block::LeftBlock(5)]),
            // [▌] LEFT 4 EIGHTHS BLOCK
            0x258c => Self::Blocks(&[Block::LeftBlock(4)]),
            // [▍] LEFT 3 EIGHTHS BLOCK
            0x258d => Self::Blocks(&[Block::LeftBlock(3)]),
            // [▎] LEFT 2 EIGHTHS BLOCK
            0x258e => Self::Blocks(&[Block::LeftBlock(2)]),
            // [▏] LEFT 1 EIGHTHS BLOCK
            0x258f => Self::Blocks(&[Block::LeftBlock(1)]),
            // [▐] RIGHT HALF BLOCK
            0x2590 => Self::Blocks(&[Block::RightBlock(4)]),
            // [░] LIGHT SHADE
            0x2591 => Self::Blocks(&[Block::Custom(0, 8, 0, 8, BlockAlpha::Light)]),
            // [▒] MEDIUM SHADE
            0x2592 => Self::Blocks(&[Block::Custom(0, 8, 0, 8, BlockAlpha::Medium)]),
            // [▓] DARK SHADE
            0x2593 => Self::Blocks(&[Block::Custom(0, 8, 0, 8, BlockAlpha::Dark)]),
            // [▔] UPPER ONE EIGHTH BLOCK
            0x2594 => Self::Blocks(&[Block::UpperBlock(1)]),
            // [▕] RIGHT ONE EIGHTH BLOCK
            0x2595 => Self::Blocks(&[Block::RightBlock(1)]),
            // [▖] QUADRANT LOWER LEFT
            0x2596 => Self::Blocks(&[Block::QuadrantLL]),
            // [▗] QUADRANT LOWER RIGHT
            0x2597 => Self::Blocks(&[Block::QuadrantLR]),
            // [▘] QUADRANT UPPER LEFT
            0x2598 => Self::Blocks(&[Block::QuadrantUL]),
            // [▙] QUADRANT UPPER LEFT AND LOWER LEFT AND LOWER RIGHT
            0x2599 => Self::Blocks(&[Block::QuadrantUL, Block::QuadrantLL, Block::QuadrantLR]),
            // [▚] QUADRANT UPPER LEFT AND LOWER RIGHT
            0x259a => Self::Blocks(&[Block::QuadrantUL, Block::QuadrantLR]),
            // [▛] QUADRANT UPPER LEFT AND UPPER RIGHT AND LOWER LEFT
            0x259b => Self::Blocks(&[Block::QuadrantUL, Block::QuadrantUR, Block::QuadrantLL]),
            // [▜] QUADRANT UPPER LEFT AND UPPER RIGHT AND LOWER RIGHT
            0x259c => Self::Blocks(&[Block::QuadrantUL, Block::QuadrantUR, Block::QuadrantLR]),
            // [▝] QUADRANT UPPER RIGHT
            0x259d => Self::Blocks(&[Block::QuadrantUR]),
            // [▞] QUADRANT UPPER RIGHT AND LOWER LEFT
            0x259e => Self::Blocks(&[Block::QuadrantUR, Block::QuadrantLL]),
            // [▟] QUADRANT UPPER RIGHT AND LOWER LEFT AND LOWER RIGHT
            0x259f => Self::Blocks(&[Block::QuadrantUR, Block::QuadrantLL, Block::QuadrantLR]),
            // Sextant blocks
            sextant @ 0x1fb00..=0x1fb3b => {
                Self::Sextant(SEXTANT_PATTERNS[(sextant & 0x3f) as usize])
            }
            // Octant blocks
            octant @ 0x1cd00..=0x1cde5 => Self::Octant(OCTANT_PATTERNS[(octant & 0xff) as usize]),
            // [𜺠] RIGHT HALF LOWER ONE QUARTER BLOCK (corresponds to OCTANT-8)
            0x1cea0 => Self::Octant(0b10000000),
            // [𜺣; EFT HALF LOWER ONE QUARTER BLOCK (corresponds to OCTANT-7)
            0x1cea3 => Self::Octant(0b01000000),
            // [𜺨] LEFT HALF UPPER ONE QUARTER BLOCK (corresponds to OCTANT-1)
            0x1cea8 => Self::Octant(0b00000001),
            // [𜺫] RIGHT HALF UPPER ONE QUARTER BLOCK (corresponds to OCTANT-2)
            0x1ceab => Self::Octant(0b00000010),
            // [🯦] MIDDLE LEFT ONE QUARTER BLOCK (corresponds to OCTANT-35)
            0x1fbe6 => Self::Octant(0b00010100),
            // [🯧] MIDDLE RIGHT ONE QUARTER BLOCK (corresponds to OCTANT-46)
            0x1fbe7 => Self::Octant(0b00101000),
            // [🬼] LOWER LEFT BLOCK DIAGONAL LOWER MIDDLE LEFT TO LOWER CENTRE
            0x1fb3c => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(2, 3)),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🬽] LOWER LEFT BLOCK DIAGONAL LOWER MIDDLE LEFT TO LOWER RIGHT
            0x1fb3d => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(2, 3)),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🬾] LOWER LEFT BLOCK DIAGONAL UPPER MIDDLE LEFT TO LOWER CENTRE
            0x1fb3e => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 3)),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🬿] LOWER LEFT BLOCK DIAGONAL UPPER MIDDLE LEFT TO LOWER RIGHT
            0x1fb3f => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 3)),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭀] LOWER LEFT BLOCK DIAGONAL UPPER LEFT TO LOWER CENTRE
            0x1fb40 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭁] LOWER RIGHT BLOCK DIAGONAL UPPER MIDDLE LEFT TO UPPER CENTRE
            0x1fb41 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭂] LOWER RIGHT BLOCK DIAGONAL UPPER MIDDLE LEFT TO UPPER RIGHT
            0x1fb42 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭃] LOWER RIGHT BLOCK DIAGONAL LOWER MIDDLE LEFT TO UPPER CENTRE
            0x1fb43 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(2, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭄] LOWER RIGHT BLOCK DIAGONAL LOWER MIDDLE LEFT TO UPPER RIGHT
            0x1fb44 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(2, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭅] LOWER RIGHT BLOCK DIAGONAL UPPER LEFT TO UPPER CENTRE
            0x1fb45 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭆] LOWER RIGHT BLOCK DIAGONAL LOWER MIDDLE LEFT TO UPPER MIDDLE RIGHT
            0x1fb46 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(2, 3)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 3)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭇] LOWER RIGHT BLOCK DIAGONAL LOWER CENTRE TO LOWER MIDDLE RIGHT
            0x1fb47 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(2, 3)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭈] LOWER RIGHT BLOCK DIAGONAL LOWER LEFT TO LOWER MIDDLE RIGHT
            0x1fb48 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(2, 3)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭉] LOWER RIGHT BLOCK DIAGONAL LOWER CENTRE TO UPPER MIDDLE RIGHT
            0x1fb49 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 3)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭊] LOWER RIGHT BLOCK DIAGONAL LOWER LEFT TO UPPER MIDDLE RIGHT
            0x1fb4a => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 3)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭋] LOWER RIGHT BLOCK DIAGONAL LOWER CENTRE TO UPPER RIGHT
            0x1fb4b => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭌] LOWER LEFT BLOCK DIAGONAL UPPER CENTRE TO UPPER MIDDLE RIGHT
            0x1fb4c => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 3)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭍] LOWER LEFT BLOCK DIAGONAL UPPER LEFT TO UPPER MIDDLE RIGHT
            0x1fb4d => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 3)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭎] LOWER LEFT BLOCK DIAGONAL UPPER CENTRE TO LOWER MIDDLE RIGHT
            0x1fb4e => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(2, 3)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭏] LOWER LEFT BLOCK DIAGONAL UPPER LEFT TO LOWER MIDDLE RIGHT
            0x1fb4f => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(2, 3)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭐] LOWER LEFT BLOCK DIAGONAL UPPER CENTRE TO LOWER RIGHT
            0x1fb50 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭑] LOWER LEFT BLOCK DIAGONAL UPPER MIDDLE LEFT TO LOWER MIDDLE RIGHT
            0x1fb51 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 3)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(2, 3)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭒] UPPER RIGHT BLOCK DIAGONAL LOWER MIDDLE LEFT TO LOWER CENTRE
            0x1fb52 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(2, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭓] UPPER RIGHT BLOCK DIAGONAL LOWER MIDDLE LEFT TO LOWER RIGHT
            0x1fb53 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(2, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭔] UPPER RIGHT BLOCK DIAGONAL UPPER MIDDLE LEFT TO LOWER CENTRE
            0x1fb54 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭕] UPPER RIGHT BLOCK DIAGONAL UPPER MIDDLE LEFT TO LOWER RIGHT
            0x1fb55 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭖] UPPER RIGHT BLOCK DIAGONAL UPPER LEFT TO LOWER CENTRE
            0x1fb56 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭗] UPPER LEFT BLOCK DIAGONAL UPPER MIDDLE LEFT TO UPPER CENTRE
            0x1fb57 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭘] UPPER LEFT BLOCK DIAGONAL UPPER MIDDLE LEFT TO UPPER RIGHT
            0x1fb58 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭙] UPPER LEFT BLOCK DIAGONAL LOWER MIDDLE LEFT TO UPPER CENTRE
            0x1fb59 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(2, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭚] UPPER LEFT BLOCK DIAGONAL LOWER MIDDLE LEFT TO UPPER RIGHT
            0x1fb5a => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(2, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭛] UPPER LEFT BLOCK DIAGONAL LOWER LEFT TO UPPER CENTRE
            0x1fb5b => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭜] UPPER LEFT BLOCK DIAGONAL LOWER MIDDLE LEFT TO UPPER MIDDLE RIGHT
            0x1fb5c => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 3)),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(2, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭝] UPPER LEFT BLOCK DIAGONAL LOWER CENTRE TO LOWER MIDDLE RIGHT
            0x1fb5d => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(2, 3)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭞] UPPER LEFT BLOCK DIAGONAL LOWER LEFT TO LOWER MIDDLE RIGHT
            0x1fb5e => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(2, 3)),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭟] UPPER LEFT BLOCK DIAGONAL LOWER CENTRE TO UPPER MIDDLE RIGHT
            0x1fb5f => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 3)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭠] UPPER LEFT BLOCK DIAGONAL LOWER LEFT TO UPPER MIDDLE RIGHT
            0x1fb60 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 3)),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭡] UPPER LEFT BLOCK DIAGONAL LOWER CENTRE TO UPPER RIGHT
            0x1fb61 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭢] UPPER RIGHT BLOCK DIAGONAL UPPER CENTRE TO UPPER MIDDLE RIGHT
            0x1fb62 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭣] UPPER RIGHT BLOCK DIAGONAL UPPER LEFT TO UPPER MIDDLE RIGHT
            0x1fb63 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭤] UPPER RIGHT BLOCK DIAGONAL UPPER CENTRE TO LOWER MIDDLE RIGHT
            0x1fb64 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(2, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭥] UPPER RIGHT BLOCK DIAGONAL UPPER LEFT TO LOWER MIDDLE RIGHT
            0x1fb65 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(2, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭦] UPPER RIGHT BLOCK DIAGONAL UPPER CENTRE TO LOWER RIGHT
            0x1fb66 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [🭧] UPPER RIGHT BLOCK DIAGONAL UPPER MIDDLE LEFT TO LOWER MIDDLE RIGHT
            0x1fb67 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(2, 3)),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 3)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            _ => {
                // When: codepoint has no geometry in this family, preserve the unsupported result.
                return None;
            }
        })
    }
}
