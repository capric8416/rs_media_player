# Port glossary

| Source/domain term | Meaning | Rust name |
|---|---|---|
| access unit | Compressed data needed to decode one output picture | `EncodedSample` / `AccessUnit` |
| Annex B | AVC/HEVC byte stream with start-code-delimited NAL units | `NalFraming::AnnexB` |
| length-prefixed sample | MP4 AVC/HEVC sample with a length before each NAL unit | `NalFraming::LengthPrefixed` |
| OBU | AV1 Open Bitstream Unit | `Obu` |
| extradata | Codec initialization records such as avcC, hvcC, av1C, esds, and dOps | `AvcConfig` / `HevcConfig` / `Av1Config` / `EsdsConfig` / `OpusConfig` |
| DPB | Decoded Picture Buffer holding reference pictures | `DecodedPictureBuffer` |
| reference frame | Previously decoded picture used by inter prediction | `ReferencePicture` |
| SPS | AVC sequence-level profile, format, dimensions and POC configuration | `SequenceParameterSet` |
| PPS | AVC picture-level entropy, reference, QP and scaling configuration | `PictureParameterSet` |
| POC | Picture order count used to order and reference decoded pictures | `PicOrderState` / `Picture::poc` |
| MMCO | AVC memory-management control operation for short/long DPB references | `MemoryManagementOperation` |
| RPS | HEVC short/long-term reference picture set replacing DPB reference membership per picture | `ShortTermRps` / `LongTermReference` |
| IRAP | HEVC intra random-access picture family including BLA, IDR and CRA | `NalHeader::is_irap` |
| picture parameters | DXVA codec state submitted for one picture | `PictureParameters` |
| slice/tile control | DXVA offsets and sizes for coded partitions | `SliceControl` / `TileControl` |
| hardware frame | A decoded picture stored in a GPU resource | `GpuFrame` |
| sync context | Fence/query and completion value associated with GPU work | `SyncToken` |
| DXVA2 | D3D9-era DirectX Video Acceleration API | `D3d9Decoder` backend |
| D3D11VA | D3D11 video decode and video processor interfaces | `D3d11Decoder` backend |
| D3D12 Video | Explicit D3D12 video queues, command lists, heaps, and fences | `D3d12Decoder` backend |
| video processor | Hardware unit used for scale, format conversion, and color processing | `D3dScale` implementation |
| NV12 | 8-bit 4:2:0 semiplanar video format | `PixelFormat::Nv12` |
| P010 | 10-bit 4:2:0 semiplanar video format in 16-bit words | `PixelFormat::P010` |
| BGRA8 | 8-bit presentation/render target format | `PixelFormat::Bgra8` |
| RGB10A2 | 10-bit RGB HDR presentation format | `PixelFormat::Rgb10A2` |
| MOV/MP4 atom or box | Length/type-delimited ISO BMFF structure | `BoxHeader` / `FourCc` |
| sample table | Tables mapping decode time, composition time, chunks, offsets, sizes, and sync samples | `ClassicTableBoxes` / `SampleIndex` |
| fragmented MP4 | MP4 using moof/traf/trun fragments rather than only a static moov table | `TrackFragmentHeader` / `FragmentRun` |
| decode timestamp | Time at which a compressed sample is decoded | `dts` in `SampleTiming` |
| presentation timestamp | Time at which a decoded sample is displayed | `pts` in `SampleTiming` |
| color space | Primaries, transfer, matrix, and range describing sample interpretation | `ColorInfo` |
| HDR10 metadata | Mastering display and content light metadata | `Hdr10Metadata` |
| device lost | GPU device removal/reset requiring application-level recovery | `VideoError::DeviceLost` |
