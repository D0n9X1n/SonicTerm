//! Rounded box corners, diagonals, and line end caps.

// Geometry from WezTerm 577474d89ee61aef4a48145cdec82a638d874751, MIT; see LICENSE-WEZTERM.
use super::*;

impl BlockKey {
    /// Map the codepoints owned by this glyph family without changing their geometry.
    pub(super) fn from_box_arcs_caps(codepoint: u32) -> Option<Self> {
        Some(match codepoint {
            // [╭] BOX DRAWINGS LIGHT ARC DOWN AND RIGHT
            0x256d => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(3, 4)),
                    PolyCommand::QuadTo {
                        control: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        to: (BlockCoord::Frac(3, 4), BlockCoord::Frac(1, 2)),
                    },
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [╮] BOX DRAWINGS LIGHT ARC DOWN AND LEFT
            0x256e => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(3, 4)),
                    PolyCommand::QuadTo {
                        control: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        to: (BlockCoord::Frac(1, 4), BlockCoord::Frac(1, 2)),
                    },
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [╯] BOX DRAWINGS LIGHT ARC UP AND LEFT
            0x256f => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 4)),
                    PolyCommand::QuadTo {
                        control: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        to: (BlockCoord::Frac(1, 4), BlockCoord::Frac(1, 2)),
                    },
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [╰] BOX DRAWINGS LIGHT ARC UP AND RIGHT
            0x2570 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 4)),
                    PolyCommand::QuadTo {
                        control: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        to: (BlockCoord::Frac(3, 4), BlockCoord::Frac(1, 2)),
                    },
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),

            // [╱] BOX DRAWINGS LIGHT DIAGONAL UPPER RIGHT TO LOWER LEFT
            0x2571 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [╲] BOX DRAWINGS LIGHT DIAGONAL UPPER LEFT TO LOWER RIGHT
            0x2572 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [╳] BOX DRAWINGS LIGHT DIAGONAL CROSS
            0x2573 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╴] BOX DRAWINGS LIGHT LEFT
            0x2574 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [╵] BOX DRAWINGS LIGHT UP
            0x2575 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [╶] BOX DRAWINGS LIGHT RIGHT
            0x2576 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [╷] BOX DRAWINGS LIGHT DOWN
            0x2577 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [╸] BOX DRAWINGS HEAVY LEFT
            0x2578 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::OutlineHeavy,
            }]),
            // [╹] BOX DRAWINGS HEAVY UP
            0x2579 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::OutlineHeavy,
            }]),
            // [╺] BOX DRAWINGS HEAVY RIGHT
            0x257a => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::OutlineHeavy,
            }]),
            // [╻] BOX DRAWINGS HEAVY DOWN
            0x257b => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::OutlineHeavy,
            }]),
            // [╼] BOX DRAWINGS LIGHT LEFT AND HEAVY RIGHT
            0x257c => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [╽] BOX DRAWINGS LIGHT UP AND HEAVY DOWN
            0x257d => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::Frac(1, 2),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Div(-1)),
                        ),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [╾] BOX DRAWINGS HEAVY LEFT AND LIGHT RIGHT
            0x257e => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╿] BOX DRAWINGS HEAVY UP AND LIGHT DOWN
            0x257f => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(
                            BlockCoord::Frac(1, 2),
                            BlockCoord::FracWithOffset(1, 2, LineScale::Div(-1)),
                        ),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
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
