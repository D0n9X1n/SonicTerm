use super::*;

#[test]
fn font_units_round_trip_positive_and_negative_values() {
    assert_eq!(FT_Pos::from_font_units(1234).font_units(), 1234);
    assert_eq!(FT_Pos::from_font_units(-1234).font_units(), -1234);
}

#[test]
fn pos_bits_are_interpreted_as_sixteen_sixteen() {
    assert_eq!(FT_Pos::from_font_units(1 << 16).f16d16().to_num::<f64>(), 1.0);
    assert_eq!(FT_Pos::from_font_units(1 << 15).f16d16().to_num::<f64>(), 0.5);
    assert_eq!(FT_Pos::from_font_units(-(1 << 16)).f16d16().to_num::<f64>(), -1.0);
}

#[test]
fn pos_bits_are_interpreted_as_twenty_six_six() {
    assert_eq!(FT_Pos::from_font_units(64).f26d6().to_num::<f64>(), 1.0);
    assert_eq!(FT_Pos::from_font_units(32).f26d6().to_num::<f64>(), 0.5);
    assert_eq!(FT_Pos::from_font_units(-96).f26d6().to_num::<f64>(), -1.5);
}

#[test]
fn fixed_conversions_preserve_raw_bits() {
    let six = FT_F26Dot6::from_bits(96);
    let pos = FT_Pos::from(six);
    assert_eq!(pos.font_units(), 96);
    assert_eq!(pos.f26d6().to_num::<f64>(), 1.5);

    let sixteen = FT_Fixed::from_bits(3 << 15);
    let pos = FT_Pos::from(sixteen);
    assert_eq!(pos.font_units(), 3 << 15);
    assert_eq!(pos.f16d16().to_num::<f64>(), 1.5);
}

#[test]
fn two_dot_fourteen_handles_positive_and_negative_fractions() {
    assert_eq!(FT_F2Dot14::from_bits(1 << 14).to_num::<f64>(), 1.0);
    assert_eq!(FT_F2Dot14::from_bits(1 << 13).to_num::<f64>(), 0.5);
    assert_eq!(FT_F2Dot14::from_bits(-(1 << 13)).to_num::<f64>(), -0.5);
}
