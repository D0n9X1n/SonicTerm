//! Powerline separators, progress indicators, spinners, and branch diagrams.

// Geometry from WezTerm 577474d89ee61aef4a48145cdec82a638d874751, MIT; see LICENSE-WEZTERM.
use super::*;

impl BlockKey {
    /// Map the codepoints owned by this glyph family without changing their geometry.
    pub(super) fn from_symbols(codepoint: u32) -> Option<Self> {
        Some(match codepoint {
            // [] Powerline filled right arrow
            0xe0b0 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [] Powerline outline right arrow
            0xe0b1 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [] Powerline filled left arrow
            0xe0b2 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [] Powerline outline left arrow
            0xe0b3 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),

            // [] Powerline filled left semicircle
            0xe0b4 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::QuadTo {
                        control: (BlockCoord::One, BlockCoord::Zero),
                        to: (BlockCoord::One, BlockCoord::Frac(1, 2)),
                    },
                    PolyCommand::QuadTo {
                        control: (BlockCoord::One, BlockCoord::One),
                        to: (BlockCoord::Zero, BlockCoord::One),
                    },
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [] Powerline outline left semicircle
            0xe0b5 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(-1, 4), BlockCoord::Frac(-1, 3)),
                    PolyCommand::QuadTo {
                        control: (BlockCoord::Frac(7, 4), BlockCoord::Frac(1, 2)),
                        to: (BlockCoord::Frac(-1, 4), BlockCoord::Frac(4, 3)),
                    },
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [] Powerline filled right semicircle
            0xe0b6 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::QuadTo {
                        control: (BlockCoord::Zero, BlockCoord::Zero),
                        to: (BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    },
                    PolyCommand::QuadTo {
                        control: (BlockCoord::Zero, BlockCoord::One),
                        to: (BlockCoord::One, BlockCoord::One),
                    },
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [] Powerline outline right semicircle
            0xe0b7 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(5, 4), BlockCoord::Frac(-1, 3)),
                    PolyCommand::QuadTo {
                        control: (BlockCoord::Frac(-3, 4), BlockCoord::Frac(1, 2)),
                        to: (BlockCoord::Frac(5, 4), BlockCoord::Frac(4, 3)),
                    },
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),

            // [] Powerline filled bottom left half triangle
            0xe0b8 => Self::Triangles(Triangle::LEFT | Triangle::LOWER, BlockAlpha::Full),
            // [] Powerline outline bottom left half triangle
            0xe0b9 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [] Powerline filled bottom right half triangle
            0xe0ba => Self::Triangles(Triangle::RIGHT | Triangle::LOWER, BlockAlpha::Full),
            // [] Powerline outline bottom right half triangle
            0xe0bb => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [] Powerline filled top left half triangle
            0xe0bc => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::Close,
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Fill,
            }]),
            // [] Powerline outline top left half triangle
            0xe0bd => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [] Powerline filled top right half triangle
            0xe0be => Self::Triangles(Triangle::RIGHT | Triangle::UPPER, BlockAlpha::Full),
            // [] Powerline outline top right half triangle
            0xe0bf => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [] Progress chunk - left empty
            0xee00 => Self::Progress(ProgressChunk::LEFT),
            // [] Progress chunk - middle empty
            0xee01 => Self::Progress(ProgressChunk::MIDDLE),
            // [] Progress chunk - right empty
            0xee02 => Self::Progress(ProgressChunk::RIGHT),
            // [] Progress chunk - left full
            0xee03 => Self::Progress(ProgressChunk::LEFT | ProgressChunk::FULL),
            // [] Progress chunk - middle full
            0xee04 => Self::Progress(ProgressChunk::MIDDLE | ProgressChunk::FULL),
            // [] Progress chunk - right full
            0xee05 => Self::Progress(ProgressChunk::RIGHT | ProgressChunk::FULL),
            spinner @ 0xee06..=0xee0b => Self::Spinner((spinner & 0xff) as u8 - 6),
            // [] Branch drawing horizontal
            0xF5D0 => Self::Branches(Branch::HORIZONTAL),
            // [] Branch drawing vertical
            0xF5D1 => Self::Branches(Branch::VERTICAL),
            // [] Branch drawing fade out to right
            0xF5D2 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(5, 30), BlockCoord::Frac(1, 2)),
                    PolyCommand::MoveTo(BlockCoord::Frac(6, 30), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(10, 30), BlockCoord::Frac(1, 2)),
                    PolyCommand::MoveTo(BlockCoord::Frac(12, 30), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(15, 30), BlockCoord::Frac(1, 2)),
                    PolyCommand::MoveTo(BlockCoord::Frac(18, 30), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(20, 30), BlockCoord::Frac(1, 2)),
                    PolyCommand::MoveTo(BlockCoord::Frac(24, 30), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(25, 30), BlockCoord::Frac(1, 2)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::OutlineHeavy,
            }]),
            // [] Branch drawing fade out to left
            0xF5D3 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(30 - 5, 30), BlockCoord::Frac(1, 2)),
                    PolyCommand::MoveTo(BlockCoord::Frac(30 - 6, 30), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(30 - 10, 30), BlockCoord::Frac(1, 2)),
                    PolyCommand::MoveTo(BlockCoord::Frac(30 - 12, 30), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(30 - 15, 30), BlockCoord::Frac(1, 2)),
                    PolyCommand::MoveTo(BlockCoord::Frac(30 - 18, 30), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(30 - 20, 30), BlockCoord::Frac(1, 2)),
                    PolyCommand::MoveTo(BlockCoord::Frac(30 - 24, 30), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(30 - 25, 30), BlockCoord::Frac(1, 2)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::OutlineHeavy,
            }]),
            // [] Branch drawing fade out to lower
            0xF5D4 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(5, 30)),
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(6, 30)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(10, 30)),
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(12, 30)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(15, 30)),
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(18, 30)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(20, 30)),
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(24, 30)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(25, 30)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::OutlineHeavy,
            }]),
            // [] Branch drawing fade out to upper
            0xF5D5 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(30 - 5, 30)),
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(30 - 6, 30)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(30 - 10, 30)),
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(30 - 12, 30)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(30 - 15, 30)),
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(30 - 18, 30)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(30 - 20, 30)),
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(30 - 24, 30)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(30 - 25, 30)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::OutlineHeavy,
            }]),
            // [] Branch drawing arc down and right
            0xF5D6 => Self::Branches(Branch::RIGHT_TO_DOWN),
            // [] Branch drawing arc down and left
            0xF5D7 => Self::Branches(Branch::LEFT_TO_DOWN),
            // [] Branch drawing arc up and right
            0xF5D8 => Self::Branches(Branch::RIGHT_TO_UP),
            // [] Branch drawing arc up and left
            0xF5D9 => Self::Branches(Branch::LEFT_TO_UP),
            // [] Branch drawing merge from lower and right
            0xF5DA => Self::Branches(Branch::VERTICAL | Branch::RIGHT_TO_UP),
            // [] Branch drawing branch to upper and right
            0xF5DB => Self::Branches(Branch::VERTICAL | Branch::RIGHT_TO_DOWN),
            // [] Branch drawing upper to right and lower to right
            0xF5DC => Self::Branches(Branch::RIGHT_TO_UP | Branch::RIGHT_TO_DOWN),
            // [] Branch drawing merge from left and down
            0xF5DD => Self::Branches(Branch::VERTICAL | Branch::LEFT_TO_UP),
            // [] Branch drawing branch to left and up
            0xF5DE => Self::Branches(Branch::VERTICAL | Branch::LEFT_TO_DOWN),
            // [] Branch drawing upper to left and lower to left
            0xF5DF => Self::Branches(Branch::LEFT_TO_UP | Branch::LEFT_TO_DOWN),
            // [] Branch drawing from left to right and left to down
            0xF5E0 => Self::Branches(Branch::HORIZONTAL | Branch::LEFT_TO_DOWN),
            // [] Branch drawing from right to left and right to down
            0xF5E1 => Self::Branches(Branch::HORIZONTAL | Branch::RIGHT_TO_DOWN),
            // [] Branch drawing from down to left and down to right
            0xF5E2 => Self::Branches(Branch::LEFT_TO_DOWN | Branch::RIGHT_TO_DOWN),
            // [] Branch drawing from left to up and left to right
            0xF5E3 => Self::Branches(Branch::HORIZONTAL | Branch::LEFT_TO_UP),
            // [] Branch drawing from right to up and right to left
            0xF5E4 => Self::Branches(Branch::HORIZONTAL | Branch::RIGHT_TO_UP),
            // [] Branch drawing from up to left and up to right
            0xF5E5 => Self::Branches(Branch::LEFT_TO_UP | Branch::RIGHT_TO_UP),
            // [] Branch drawing from up to left, up to right, and up to down
            0xF5E6 => Self::Branches(Branch::VERTICAL | Branch::LEFT_TO_UP | Branch::RIGHT_TO_UP),
            // [] Branch drawing from down to left, down to right, and down to up
            0xF5E7 => {
                Self::Branches(Branch::VERTICAL | Branch::LEFT_TO_DOWN | Branch::RIGHT_TO_DOWN)
            }
            // [] Branch drawing from left to right, left to up, and left to down
            0xF5E8 => {
                Self::Branches(Branch::HORIZONTAL | Branch::LEFT_TO_UP | Branch::LEFT_TO_DOWN)
            }
            // [] Branch drawing from right to left, right to up, and right to down
            0xF5E9 => {
                Self::Branches(Branch::HORIZONTAL | Branch::RIGHT_TO_DOWN | Branch::RIGHT_TO_UP)
            }
            // [] Branch drawing from up to down, up to left, and down to right
            0xF5EA => Self::Branches(Branch::VERTICAL | Branch::LEFT_TO_UP | Branch::RIGHT_TO_DOWN),
            // [] Branch drawing from up to down, up to right, and down to left
            0xF5EB => Self::Branches(Branch::VERTICAL | Branch::LEFT_TO_DOWN | Branch::RIGHT_TO_UP),
            // [] Branch drawing from left to right, left to up, and right to down
            0xF5EC => {
                Self::Branches(Branch::HORIZONTAL | Branch::LEFT_TO_UP | Branch::RIGHT_TO_DOWN)
            }
            // [] Branch drawing from left to right, left to down, and right to up
            0xF5ED => {
                Self::Branches(Branch::HORIZONTAL | Branch::LEFT_TO_DOWN | Branch::RIGHT_TO_UP)
            }
            // [] Branch drawing filled circle
            0xF5EE => Self::Branches(Branch::CIRCLE_FILLED),
            // [] Branch drawing outline circle
            0xF5EF => Self::Branches(Branch::CIRCLE_OUTLINE),
            // [] Branch drawing filled circle connected to right
            0xF5F0 => Self::Branches(Branch::CIRCLE_FILLED | Branch::RIGHT),
            // [] Branch drawing outline circle connected to right
            0xF5F1 => Self::Branches(Branch::CIRCLE_OUTLINE | Branch::RIGHT),
            // [] Branch drawing filled circle connected to left
            0xF5F2 => Self::Branches(Branch::CIRCLE_FILLED | Branch::LEFT),
            // [] Branch drawing outline circle connected to left
            0xF5F3 => Self::Branches(Branch::CIRCLE_OUTLINE | Branch::LEFT),
            // [] Branch drawing filled circle connected to left and right
            0xF5F4 => Self::Branches(Branch::CIRCLE_FILLED | Branch::LEFT | Branch::RIGHT),
            // [] Branch drawing outline circle connected to left and right
            0xF5F5 => Self::Branches(Branch::CIRCLE_OUTLINE | Branch::LEFT | Branch::RIGHT),
            // [] Branch drawing filled circle connected to down
            0xF5F6 => Self::Branches(Branch::CIRCLE_FILLED | Branch::DOWN),
            // [] Branch drawing outline circle connected to down
            0xF5F7 => Self::Branches(Branch::CIRCLE_OUTLINE | Branch::DOWN),
            // [] Branch drawing filled circle connected to up
            0xF5F8 => Self::Branches(Branch::CIRCLE_FILLED | Branch::UP),
            // [] Branch drawing outline circle connected to up
            0xF5F9 => Self::Branches(Branch::CIRCLE_OUTLINE | Branch::UP),
            // [] Branch drawing filled circle connected to up and down
            0xF5FA => Self::Branches(Branch::CIRCLE_FILLED | Branch::UP | Branch::DOWN),
            // [] Branch drawing outline circle connected to up and down
            0xF5FB => Self::Branches(Branch::CIRCLE_OUTLINE | Branch::UP | Branch::DOWN),
            // [] Branch drawing filled circle connected to right and down
            0xF5FC => Self::Branches(Branch::CIRCLE_FILLED | Branch::RIGHT | Branch::DOWN),
            // [] Branch drawing outline circle connected to right and down
            0xF5FD => Self::Branches(Branch::CIRCLE_OUTLINE | Branch::RIGHT | Branch::DOWN),
            // [] Branch drawing filled circle connected to left and down
            0xF5FE => Self::Branches(Branch::CIRCLE_FILLED | Branch::LEFT | Branch::DOWN),
            // [] Branch drawing outline circle connected to left and down
            0xF5FF => Self::Branches(Branch::CIRCLE_OUTLINE | Branch::LEFT | Branch::DOWN),
            // [] Branch drawing filled circle connected to right and up
            0xF600 => Self::Branches(Branch::CIRCLE_FILLED | Branch::RIGHT | Branch::UP),
            // [] Branch drawing outline circle connected to right and up
            0xF601 => Self::Branches(Branch::CIRCLE_OUTLINE | Branch::RIGHT | Branch::UP),
            // [] Branch drawing filled circle connected to left and up
            0xF602 => Self::Branches(Branch::CIRCLE_FILLED | Branch::LEFT | Branch::UP),
            // [] Branch drawing outline circle connected to left and up
            0xF603 => Self::Branches(Branch::CIRCLE_OUTLINE | Branch::LEFT | Branch::UP),
            // [] Branch drawing filled circle connected to right, up, and down
            0xF604 => {
                Self::Branches(Branch::CIRCLE_FILLED | Branch::RIGHT | Branch::UP | Branch::DOWN)
            }
            // [] Branch drawing outline circle connected to right, up, and down
            0xF605 => {
                Self::Branches(Branch::CIRCLE_OUTLINE | Branch::RIGHT | Branch::UP | Branch::DOWN)
            }
            // [] Branch drawing filled circle connected to left, up, and down
            0xF606 => {
                Self::Branches(Branch::CIRCLE_FILLED | Branch::LEFT | Branch::UP | Branch::DOWN)
            }
            // [] Branch drawing outline circle connected to left, up, and down
            0xF607 => {
                Self::Branches(Branch::CIRCLE_OUTLINE | Branch::LEFT | Branch::UP | Branch::DOWN)
            }
            // [] Branch drawing filled circle connected to left, right, and down
            0xF608 => {
                Self::Branches(Branch::CIRCLE_FILLED | Branch::LEFT | Branch::RIGHT | Branch::DOWN)
            }
            // [] Branch drawing outline circle connected to left, right, and down
            0xF609 => {
                Self::Branches(Branch::CIRCLE_OUTLINE | Branch::LEFT | Branch::RIGHT | Branch::DOWN)
            }
            // [] Branch drawing filled circle connected to left, right, and up
            0xF60A => {
                Self::Branches(Branch::CIRCLE_FILLED | Branch::LEFT | Branch::RIGHT | Branch::UP)
            }
            // [] Branch drawing outline circle connected to left, right, and up
            0xF60B => {
                Self::Branches(Branch::CIRCLE_OUTLINE | Branch::LEFT | Branch::RIGHT | Branch::UP)
            }
            // [] Branch drawing filled circle connected to left, right, up, and down
            0xF60C => Self::Branches(
                Branch::CIRCLE_FILLED | Branch::LEFT | Branch::RIGHT | Branch::UP | Branch::DOWN,
            ),
            // [] Branch drawing outline circle connected to left, right, up, and down
            0xF60D => Self::Branches(
                Branch::CIRCLE_OUTLINE | Branch::LEFT | Branch::RIGHT | Branch::UP | Branch::DOWN,
            ),
            _ => {
                // When: codepoint has no geometry in this family, preserve the unsupported result.
                return None;
            }
        })
    }
}
