//! Double-line box-drawing tees and crosses.

// Geometry from WezTerm 577474d89ee61aef4a48145cdec82a638d874751, MIT; see LICENSE-WEZTERM.
use super::*;

impl BlockKey {
    /// Map the codepoints owned by this glyph family without changing their geometry.
    pub(super) fn from_box_double_junctions(codepoint: u32) -> Option<Self> {
        Some(match codepoint {
            // [╞] BOX DRAWINGS VERTICAL SINGLE AND RIGHT DOUBLE
            0x255e => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(
                            BlockCoord::Frac(1, 2),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::One,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::Frac(1, 2),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::Frac(1, 2),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::One,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╟] BOX DRAWINGS VERTICAL DOUBLE AND RIGHT SINGLE
            0x255f => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::Zero,
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::One,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::Zero,
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::Frac(1, 2),
                        ),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::Frac(1, 2),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::One,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),

            // [╠] BOX DRAWINGS DOUBLE VERTICAL AND RIGHT
            0x2560 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::Zero,
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::One,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::Zero,
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::One,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::One,
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::One,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╡] BOX DRAWINGS VERTICAL SINGLE AND LEFT DOUBLE
            0x2561 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(
                            BlockCoord::Frac(1, 2),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::Zero,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::Frac(1, 2),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::Frac(1, 2),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::Zero,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╢] BOX DRAWINGS VERTICAL DOUBLE AND LEFT SINGLE
            0x2562 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::Zero,
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::One,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::Zero,
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::Frac(1, 2),
                        ),
                        PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::Frac(1, 2),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::One,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╣] BOX DRAWINGS DOUBLE VERTICAL AND LEFT
            0x2563 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::Zero,
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::One,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::Zero,
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::Zero,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::One,
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::Zero,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╤] BOX DRAWINGS DOWN SINGLE AND HORIZONTAL DOUBLE
            0x2564 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::Zero,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::One,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::Zero,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::Frac(1, 2),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                        PolyCommand::LineTo(
                            BlockCoord::Frac(1, 2),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::One,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╥] BOX DRAWINGS DOWN DOUBLE AND HORIZONTAL SINGLE
            0x2565 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::Frac(1, 2),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::One,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::Frac(1, 2),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::One,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╦] BOX DRAWINGS DOUBLE DOWN AND HORIZONTAL
            0x2566 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::Zero,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::One,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::Zero,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::One,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::One,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::One,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╧] BOX DRAWINGS UP SINGLE AND HORIZONTAL DOUBLE
            0x2567 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::Zero,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::One,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::Zero,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::Frac(1, 2),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(
                            BlockCoord::Frac(1, 2),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::One,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╨] BOX DRAWINGS UP DOUBLE AND HORIZONTAL SINGLE
            0x2568 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::Frac(1, 2),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::Zero,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::Frac(1, 2),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::Zero,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╩] BOX DRAWINGS DOUBLE UP AND HORIZONTAL
            0x2569 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::Zero,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::One,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::Zero,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::Zero,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::One,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::Zero,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╪] BOX DRAWINGS VERTICAL SINGLE AND HORIZONTAL DOUBLE
            0x256a => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::Zero,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::One,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::Zero,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::One,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╫] BOX DRAWINGS VERTICAL DOUBLE AND HORIZONTAL SINGLE
            0x256b => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::Zero,
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::One,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::Zero,
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::One,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),

            // [╬] BOX DRAWINGS DOUBLE VERTICAL AND HORIZONTAL
            0x256c => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::Zero,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::Zero,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::One,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::Zero,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::Zero,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-1)),
                            BlockCoord::One,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::One,
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                        ),
                        PolyCommand::LineTo(
                            BlockCoord::FracWithOffset(1, 2, LineScale::Mul(1)),
                            BlockCoord::One,
                        ),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),

            _ => {
                // When: codepoint has no geometry in this family, preserve the unsupported result.
                return None;
            }
        })
    }
}
