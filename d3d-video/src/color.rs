/// Color primaries carried by the compressed stream or container.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum ColorPrimaries {
    #[default]
    Unspecified,
    Bt709,
    Bt470M,
    Bt470Bg,
    Smpte170M,
    Smpte240M,
    Bt2020,
    DisplayP3,
}

/// Opto-electronic/electro-optical transfer characteristic.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum TransferCharacteristic {
    #[default]
    Unspecified,
    Bt709,
    Gamma22,
    Gamma28,
    Smpte170M,
    Smpte240M,
    Linear,
    Srgb,
    Bt2020TenBit,
    Smpte2084,
    AribStdB67,
}

/// YUV-to-RGB matrix coefficients.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum ColorMatrix {
    Identity,
    Bt709,
    Fcc,
    Bt470Bg,
    Smpte170M,
    Smpte240M,
    Bt2020NonConstant,
    Bt2020Constant,
    #[default]
    Unspecified,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum ColorRange {
    #[default]
    Unspecified,
    Limited,
    Full,
}

/// Chromaticity coordinate in the CTA/HEVC fixed-point representation (0.00002 units).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Chromaticity {
    pub x: u16,
    pub y: u16,
}

/// Mastering display metadata using the integer units defined by HDR10 carriage formats.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MasteringDisplayMetadata {
    pub red: Chromaticity,
    pub green: Chromaticity,
    pub blue: Chromaticity,
    pub white_point: Chromaticity,
    /// Maximum luminance in 0.0001 cd/m² units.
    pub max_luminance: u32,
    /// Minimum luminance in 0.0001 cd/m² units.
    pub min_luminance: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Hdr10Metadata {
    pub mastering_display: Option<MasteringDisplayMetadata>,
    pub max_content_light_level: Option<u16>,
    pub max_frame_average_light_level: Option<u16>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ColorInfo {
    pub primaries: ColorPrimaries,
    pub transfer: TransferCharacteristic,
    pub matrix: ColorMatrix,
    pub range: ColorRange,
    pub hdr10: Option<Hdr10Metadata>,
}

pub(crate) const fn bgra8_output_color() -> ColorInfo {
    ColorInfo {
        primaries: ColorPrimaries::Bt709,
        transfer: TransferCharacteristic::Gamma22,
        matrix: ColorMatrix::Identity,
        range: ColorRange::Full,
        hdr10: None,
    }
}
