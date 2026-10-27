//! Legacy mosaic triangles, stripes, diagonals, and Braille patterns.

// Geometry from WezTerm 577474d89ee61aef4a48145cdec82a638d874751, MIT; see LICENSE-WEZTERM.
use super::*;

impl BlockKey {
    /// Map the codepoints owned by this glyph family without changing their geometry.
    pub(super) fn from_legacy_mosaics(codepoint: u32) -> Option<Self> {
        Some(match codepoint {
            // [🭨] UPPER AND RIGHT AND LOWER TRIANGULAR THREE QUARTERS BLOCK
            0x1fb68 => Self::Triangles(
                Triangle::UPPER | Triangle::RIGHT | Triangle::LOWER,
                BlockAlpha::Full,
            ),
            // [🭩] LEFT AND LOWER AND RIGHT TRIANGULAR THREE QUARTERS BLOCK
            0x1fb69 => Self::Triangles(
                Triangle::LEFT | Triangle::LOWER | Triangle::RIGHT,
                BlockAlpha::Full,
            ),
            // [🭪] UPPER AND LEFT AND LOWER TRIANGULAR THREE QUARTERS BLOCK
            0x1fb6a => Self::Triangles(
                Triangle::UPPER | Triangle::LEFT | Triangle::LOWER,
                BlockAlpha::Full,
            ),
            // [🭫] LEFT AND UPPER AND RIGHT TRIANGULAR THREE QUARTERS BLOCK
            0x1fb6b => Self::Triangles(
                Triangle::LEFT | Triangle::UPPER | Triangle::RIGHT,
                BlockAlpha::Full,
            ),
            // [🭬] LEFT TRIANGULAR ONE QUARTER BLOCK
            0x1fb6c => Self::Triangles(Triangle::LEFT, BlockAlpha::Full),
            // [🭭] UPPER TRIANGULAR ONE QUARTER BLOCK
            0x1fb6d => Self::Triangles(Triangle::UPPER, BlockAlpha::Full),
            // [🭮] RIGHT TRIANGULAR ONE QUARTER BLOCK
            0x1fb6e => Self::Triangles(Triangle::RIGHT, BlockAlpha::Full),
            // [🭯] LOWER TRIANGULAR ONE QUARTER BLOCK
            0x1fb6f => Self::Triangles(Triangle::LOWER, BlockAlpha::Full),
            // [🭰] VERTICAL ONE EIGHTH BLOCK-2
            0x1fb70 => Self::Blocks(&[Block::VerticalBlock(1, 2)]),
            // [🭱] VERTICAL ONE EIGHTH BLOCK-3
            0x1fb71 => Self::Blocks(&[Block::VerticalBlock(2, 3)]),
            // [🭲] VERTICAL ONE EIGHTH BLOCK-4
            0x1fb72 => Self::Blocks(&[Block::VerticalBlock(3, 4)]),
            // [🭳] VERTICAL ONE EIGHTH BLOCK-5
            0x1fb73 => Self::Blocks(&[Block::VerticalBlock(4, 5)]),
            // [🭴] VERTICAL ONE EIGHTH BLOCK-6
            0x1fb74 => Self::Blocks(&[Block::VerticalBlock(5, 6)]),
            // [🭵] VERTICAL ONE EIGHTH BLOCK-7
            0x1fb75 => Self::Blocks(&[Block::VerticalBlock(6, 7)]),
            // [🭶] HORIZONTAL ONE EIGHTH BLOCK-2
            0x1fb76 => Self::Blocks(&[Block::HorizontalBlock(1, 2)]),
            // [🭷] HORIZONTAL ONE EIGHTH BLOCK-3
            0x1fb77 => Self::Blocks(&[Block::HorizontalBlock(2, 3)]),
            // [🭸] HORIZONTAL ONE EIGHTH BLOCK-4
            0x1fb78 => Self::Blocks(&[Block::HorizontalBlock(3, 4)]),
            // [🭹] HORIZONTAL ONE EIGHTH BLOCK-5
            0x1fb79 => Self::Blocks(&[Block::HorizontalBlock(4, 5)]),
            // [🭺] HORIZONTAL ONE EIGHTH BLOCK-6
            0x1fb7a => Self::Blocks(&[Block::HorizontalBlock(5, 6)]),
            // [🭻] HORIZONTAL ONE EIGHTH BLOCK-7
            0x1fb7b => Self::Blocks(&[Block::HorizontalBlock(6, 7)]),
            // [🭼] Left and lower one eighth block
            0x1fb7c => Self::Blocks(&[Block::LeftBlock(1), Block::LowerBlock(1)]),
            // [🭽] Left and upper one eighth block
            0x1fb7d => Self::Blocks(&[Block::LeftBlock(1), Block::UpperBlock(1)]),
            // [🭾] Right and upper one eighth block
            0x1fb7e => Self::Blocks(&[Block::RightBlock(1), Block::UpperBlock(1)]),
            // [🭿] Right and lower one eighth block
            0x1fb7f => Self::Blocks(&[Block::RightBlock(1), Block::LowerBlock(1)]),
            // [🮀] UPPER AND LOWER ONE EIGHTH BLOCK
            0x1fb80 => Self::Blocks(&[Block::UpperBlock(1), Block::LowerBlock(1)]),
            // [🮁] HORIZONTAL ONE EIGHTH BLOCK-1358
            0x1fb81 => Self::Blocks(&[
                Block::UpperBlock(1),
                Block::HorizontalBlock(2, 3),
                Block::HorizontalBlock(4, 5),
                Block::LowerBlock(1),
            ]),
            // [🮂] Upper One Quarter Block
            0x1fb82 => Self::Blocks(&[Block::UpperBlock(2)]),
            // [🮃] Upper three eighths block
            0x1fb83 => Self::Blocks(&[Block::UpperBlock(3)]),
            // [🮄] Upper five eighths block
            0x1fb84 => Self::Blocks(&[Block::UpperBlock(5)]),
            // [🮅] Upper three quarters block
            0x1fb85 => Self::Blocks(&[Block::UpperBlock(6)]),
            // [🮆] Upper seven eighths block
            0x1fb86 => Self::Blocks(&[Block::UpperBlock(7)]),
            // [🮇] Right One Quarter Block
            0x1fb87 => Self::Blocks(&[Block::RightBlock(2)]),
            // [🮈] Right three eighths block
            0x1fb88 => Self::Blocks(&[Block::RightBlock(3)]),
            // [🮉] Right five eighths block
            0x1fb89 => Self::Blocks(&[Block::RightBlock(5)]),
            // [🮊] Right three quarters block
            0x1fb8a => Self::Blocks(&[Block::RightBlock(6)]),
            // [🮋] Right seven eighths block
            0x1fb8b => Self::Blocks(&[Block::RightBlock(7)]),
            // [🮌] LEFT HALF MEDIUM SHADE
            0x1fb8c => Self::Blocks(&[Block::Custom(0, 4, 0, 8, BlockAlpha::Medium)]),
            // [🮍] RIGHT HALF MEDIUM SHADE
            0x1fb8d => Self::Blocks(&[Block::Custom(4, 8, 0, 8, BlockAlpha::Medium)]),
            // [🮎] UPPER HALF MEDIUM SHADE
            0x1fb8e => Self::Blocks(&[Block::Custom(0, 8, 0, 4, BlockAlpha::Medium)]),
            // [🮏] LOWER HALF MEDIUM SHADE
            0x1fb8f => Self::Blocks(&[Block::Custom(0, 8, 4, 8, BlockAlpha::Medium)]),
            // [🮐] INVERSE MEDIUM SHADE
            0x1fb90 => Self::Blocks(&[Block::Custom(0, 8, 0, 8, BlockAlpha::Medium)]),
            // [🮑] UPPER HALF BLOCK AND LOWER HALF INVERSE MEDIUM SHADE
            0x1fb91 => {
                Self::Blocks(&[Block::UpperBlock(4), Block::Custom(0, 8, 4, 8, BlockAlpha::Medium)])
            }
            // [🮒] UPPER HALF INVERSE MEDIUM SHADE AND LOWER HALF BLOCK
            0x1fb92 => {
                Self::Blocks(&[Block::Custom(0, 8, 0, 4, BlockAlpha::Medium), Block::LowerBlock(4)])
            }
            // [🮓] LEFT HALF BLOCK AND RIGHT HALF INVERSE MEDIUM SHADE
            // NOTE: not official!
            0x1fb93 => {
                Self::Blocks(&[Block::LeftBlock(4), Block::Custom(4, 8, 0, 8, BlockAlpha::Medium)])
            }
            // [🮔] LEFT HALF INVERSE MEDIUM SHADE AND RIGHT HALF BLOCK
            0x1fb94 => {
                Self::Blocks(&[Block::Custom(0, 4, 0, 8, BlockAlpha::Medium), Block::RightBlock(4)])
            }
            // [🮕] CHECKER BOARD FILL
            0x1fb95 => Self::Blocks(&[
                Block::Custom(0, 2, 0, 2, BlockAlpha::Full),
                Block::Custom(0, 2, 4, 6, BlockAlpha::Full),
                Block::Custom(2, 4, 2, 4, BlockAlpha::Full),
                Block::Custom(2, 4, 6, 8, BlockAlpha::Full),
                Block::Custom(4, 6, 0, 2, BlockAlpha::Full),
                Block::Custom(4, 6, 4, 6, BlockAlpha::Full),
                Block::Custom(6, 8, 2, 4, BlockAlpha::Full),
                Block::Custom(6, 8, 6, 8, BlockAlpha::Full),
            ]),
            // [🮖] INVERSE CHECKER BOARD FILL
            0x1fb96 => Self::Blocks(&[
                Block::Custom(0, 2, 2, 4, BlockAlpha::Full),
                Block::Custom(0, 2, 6, 8, BlockAlpha::Full),
                Block::Custom(2, 4, 0, 2, BlockAlpha::Full),
                Block::Custom(2, 4, 4, 6, BlockAlpha::Full),
                Block::Custom(4, 6, 2, 4, BlockAlpha::Full),
                Block::Custom(4, 6, 6, 8, BlockAlpha::Full),
                Block::Custom(6, 8, 0, 2, BlockAlpha::Full),
                Block::Custom(6, 8, 4, 6, BlockAlpha::Full),
            ]),
            // [🮗] HEAVY HORIZONTAL FILL
            0x1fb97 => Self::Blocks(&[Block::HorizontalBlock(2, 4), Block::HorizontalBlock(6, 8)]),
            // [🮘] UPPER LEFT TO LOWER RIGHT FILL
            // NOTE: This is a quick placeholder which doesn't scale correctly
            0x1fb98 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 10)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 6), BlockCoord::Zero),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineThin,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(3, 10)),
                        PolyCommand::LineTo(BlockCoord::Frac(3, 6), BlockCoord::Zero),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineThin,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(5, 10)),
                        PolyCommand::LineTo(BlockCoord::Frac(5, 6), BlockCoord::Zero),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineThin,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(7, 10)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 10)),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineThin,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(9, 10)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(3, 10)),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineThin,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 6), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(5, 10)),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineThin,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(3, 6), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(7, 10)),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineThin,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(5, 6), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(9, 10)),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineThin,
                },
            ]),
            // [🮙] UPPER RIGHT TO LOWER LEFT FILL
            // NOTE: This is a quick placeholder which doesn't scale correctly
            0x1fb99 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(1, 10)),
                        PolyCommand::LineTo(BlockCoord::Frac(5, 6), BlockCoord::Zero),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineThin,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(3, 10)),
                        PolyCommand::LineTo(BlockCoord::Frac(3, 6), BlockCoord::Zero),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineThin,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(5, 10)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 6), BlockCoord::Zero),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineThin,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(7, 10)),
                        PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 10)),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineThin,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(9, 10)),
                        PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(3, 10)),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineThin,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(5, 6), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(5, 10)),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineThin,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(3, 6), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(7, 10)),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineThin,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 6), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(9, 10)),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineThin,
                },
            ]),
            // [🮚] UPPER AND LOWER TRIANGULAR HALF BLOCK
            0x1fb9a => Self::Triangles(Triangle::UPPER | Triangle::LOWER, BlockAlpha::Full),
            // [🮛] LEFT AND RIGHT TRIANGULAR HALF BLOCK
            0x1fb9b => Self::Triangles(Triangle::LEFT | Triangle::RIGHT, BlockAlpha::Full),
            // [🮜] UPPER UPPER LEFT TRIANGULAR MEDIUM SHADE
            0x1fb9c => Self::Triangles(Triangle::LEFT | Triangle::UPPER, BlockAlpha::Medium),
            // [🮝] UPPER RIGHT TRIANGULAR MEDIUM SHADE
            0x1fb9d => Self::Triangles(Triangle::RIGHT | Triangle::UPPER, BlockAlpha::Medium),
            // [🮞] LOWER RIGHT TRIANGULAR MEDIUM SHADE
            0x1fb9e => Self::Triangles(Triangle::RIGHT | Triangle::LOWER, BlockAlpha::Medium),
            // [🮟] LOWER LEFT TRIANGULAR MEDIUM SHADE
            0x1fb9f => Self::Triangles(Triangle::LEFT | Triangle::LOWER, BlockAlpha::Medium),
            // [🮠] BOX DRAWINGS LIGHT DIAGONAL UPPER CENTRE TO MIDDLE LEFT
            0x1fba0 => Self::CellDiagonals(CellDiagonal::UPPER_LEFT),
            // [🮡] BOX DRAWINGS LIGHT DIAGONAL UPPER CENTRE TO MIDDLE RIGHT
            0x1fba1 => Self::CellDiagonals(CellDiagonal::UPPER_RIGHT),
            // [🮢] BOX DRAWINGS LIGHT DIAGONAL MIDDLE LEFT TO LOWER CENTRE
            0x1fba2 => Self::CellDiagonals(CellDiagonal::LOWER_LEFT),
            // [🮣] BOX DRAWINGS LIGHT DIAGONAL MIDDLE RIGHT TO LOWER CENTRE
            0x1fba3 => Self::CellDiagonals(CellDiagonal::LOWER_RIGHT),
            // [🮤] BOX DRAWINGS LIGHT DIAGONAL UPPER CENTRE TO MIDDLE LEFT TO LOWER CENTRE
            0x1fba4 => Self::CellDiagonals(CellDiagonal::UPPER_LEFT | CellDiagonal::LOWER_LEFT),
            // [🮥] BOX DRAWINGS LIGHT DIAGONAL UPPER CENTRE TO MIDDLE RIGHT TO LOWER CENTRE
            0x1fba5 => Self::CellDiagonals(CellDiagonal::UPPER_RIGHT | CellDiagonal::LOWER_RIGHT),
            // [🮦] BOX DRAWINGS LIGHT DIAGONAL MIDDLE LEFT TO LOWER CENTRE TO MIDDLE RIGHT
            0x1fba6 => Self::CellDiagonals(CellDiagonal::LOWER_LEFT | CellDiagonal::LOWER_RIGHT),
            // [🮧] BOX DRAWINGS LIGHT DIAGONAL MIDDLE LEFT TO UPPER CENTRE TO MIDDLE RIGHT
            0x1fba7 => Self::CellDiagonals(CellDiagonal::UPPER_LEFT | CellDiagonal::UPPER_RIGHT),
            // [🮨] BOX DRAWINGS LIGHT DIAGONAL UPPER CENTRE TO MIDDLE LEFT AND MIDDLE RIGHT TO LOWER CENTRE
            0x1fba8 => Self::CellDiagonals(CellDiagonal::UPPER_LEFT | CellDiagonal::LOWER_RIGHT),
            // [🮩] BOX DRAWINGS LIGHT DIAGONAL UPPER CENTRE TO MIDDLE RIGHT AND MIDDLE LEFT TO LOWER CENTRE
            0x1fba9 => Self::CellDiagonals(CellDiagonal::UPPER_RIGHT | CellDiagonal::LOWER_LEFT),
            // [🮪] BOX DRAWINGS LIGHT DIAGONAL UPPER CENTRE TO MIDDLE RIGHT TO LOWER CENTRE TO MIDDLE LEFT
            0x1fbaa => Self::CellDiagonals(
                CellDiagonal::UPPER_RIGHT | CellDiagonal::LOWER_LEFT | CellDiagonal::LOWER_RIGHT,
            ),
            // [🮫] BOX DRAWINGS LIGHT DIAGONAL UPPER CENTRE TO MIDDLE LEFT TO LOWER CENTRE TO MIDDLE RIGHT
            0x1fbab => Self::CellDiagonals(
                CellDiagonal::UPPER_LEFT | CellDiagonal::LOWER_LEFT | CellDiagonal::LOWER_RIGHT,
            ),
            // [🮬] BOX DRAWINGS LIGHT DIAGONAL MIDDLE LEFT TO UPPER CENTRE TO MIDDLE RIGHT TO LOWER CENTRE
            0x1fbac => Self::CellDiagonals(
                CellDiagonal::UPPER_LEFT | CellDiagonal::UPPER_RIGHT | CellDiagonal::LOWER_RIGHT,
            ),
            // [🮭] BOX DRAWINGS LIGHT DIAGONAL MIDDLE RIGHT TO UPPER CENTRE TO MIDDLE LEFT TO LOWER CENTRE
            0x1fbad => Self::CellDiagonals(
                CellDiagonal::UPPER_LEFT | CellDiagonal::UPPER_RIGHT | CellDiagonal::LOWER_LEFT,
            ),
            // [🮮] BOX DRAWINGS LIGHT DIAGONAL DIAMOND
            0x1fbae => Self::CellDiagonals(
                CellDiagonal::UPPER_LEFT
                    | CellDiagonal::UPPER_RIGHT
                    | CellDiagonal::LOWER_LEFT
                    | CellDiagonal::LOWER_RIGHT,
            ),
            // [🮯] BOX DRAWINGS LIGHT HORIZONTAL WITH VERTICAL STROKE
            0x1fbaf => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                        PolyCommand::Close,
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),

            // Braille dot patterns
            // ⠀ ⠁ ⠂ ⠃ ⠄ ⠅ ⠆ ⠇ ⠈ ⠉ ⠊ ⠋ ⠌ ⠍ ⠎ ⠏
            // ⠐ ⠑ ⠒ ⠓ ⠔ ⠕ ⠖ ⠗ ⠘ ⠙ ⠚ ⠛ ⠜ ⠝ ⠞ ⠟
            // ⠠ ⠡ ⠢ ⠣ ⠤ ⠥ ⠦ ⠧ ⠨ ⠩ ⠪ ⠫ ⠬ ⠭ ⠮ ⠯
            // ⠰ ⠱ ⠲ ⠳ ⠴ ⠵ ⠶ ⠷ ⠸ ⠹ ⠺ ⠻ ⠼ ⠽ ⠾ ⠿
            // ⡀ ⡁ ⡂ ⡃ ⡄ ⡅ ⡆ ⡇ ⡈ ⡉ ⡊ ⡋ ⡌ ⡍ ⡎ ⡏
            // ⡐ ⡑ ⡒ ⡓ ⡔ ⡕ ⡖ ⡗ ⡘ ⡙ ⡚ ⡛ ⡜ ⡝ ⡞ ⡟
            // ⡠ ⡡ ⡢ ⡣ ⡤ ⡥ ⡦ ⡧ ⡨ ⡩ ⡪ ⡫ ⡬ ⡭ ⡮ ⡯
            // ⡰ ⡱ ⡲ ⡳ ⡴ ⡵ ⡶ ⡷ ⡸ ⡹ ⡺ ⡻ ⡼ ⡽ ⡾ ⡿
            // ⢀ ⢁ ⢂ ⢃ ⢄ ⢅ ⢆ ⢇ ⢈ ⢉ ⢊ ⢋ ⢌ ⢍ ⢎ ⢏
            // ⢐ ⢑ ⢒ ⢓ ⢔ ⢕ ⢖ ⢗ ⢘ ⢙ ⢚ ⢛ ⢜ ⢝ ⢞ ⢟
            // ⢠ ⢡ ⢢ ⢣ ⢤ ⢥ ⢦ ⢧ ⢨ ⢩ ⢪ ⢫ ⢬ ⢭ ⢮ ⢯
            // ⢰ ⢱ ⢲ ⢳ ⢴ ⢵ ⢶ ⢷ ⢸ ⢹ ⢺ ⢻ ⢼ ⢽ ⢾ ⢿
            // ⣀ ⣁ ⣂ ⣃ ⣄ ⣅ ⣆ ⣇ ⣈ ⣉ ⣊ ⣋ ⣌ ⣍ ⣎ ⣏
            // ⣐ ⣑ ⣒ ⣓ ⣔ ⣕ ⣖ ⣗ ⣘ ⣙ ⣚ ⣛ ⣜ ⣝ ⣞ ⣟
            // ⣠ ⣡ ⣢ ⣣ ⣤ ⣥ ⣦ ⣧ ⣨ ⣩ ⣪ ⣫ ⣬ ⣭ ⣮ ⣯
            // ⣰ ⣱ ⣲ ⣳ ⣴ ⣵ ⣶ ⣷ ⣸ ⣹ ⣺ ⣻ ⣼ ⣽ ⣾ ⣿
            braille @ 0x2800..=0x28ff => Self::Braille((braille & 0xff) as u8),
            _ => {
                // When: codepoint has no geometry in this family, preserve the unsupported result.
                return None;
            }
        })
    }
}
