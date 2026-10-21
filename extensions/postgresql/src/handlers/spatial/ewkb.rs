use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use tokio_postgres::types::{FromSql, Kind, Type};

use super::SpatialError;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SpatialValue {
    pub kind: String,
    pub native_type: String,
    pub srid: Option<i32>,
    pub dimensions: String,
    pub encoding: String,
    pub value: String,
    pub is_empty: bool,
}

#[derive(Clone, Debug)]
pub struct EwkbInfo {
    pub srid: Option<i32>,
    pub dimensions: &'static str,
    pub positions: usize,
    pub supported: bool,
    pub invalid_xy: bool,
}

impl SpatialValue {
    pub fn from_bytes(native_type: &str, bytes: &[u8]) -> Result<Self, SpatialError> {
        let info = inspect(bytes)?;
        Ok(Self {
            kind: "spatial".into(), native_type: native_type.into(), srid: info.srid,
            dimensions: info.dimensions.into(), encoding: "ewkb-base64".into(),
            value: STANDARD.encode(bytes), is_empty: info.positions == 0,
        })
    }

    pub fn bytes(&self) -> Result<(Vec<u8>, EwkbInfo), SpatialError> {
        if self.kind != "spatial" || self.encoding != "ewkb-base64"
            || !matches!(self.native_type.as_str(), "geometry" | "geography") {
            return Err(SpatialError::argument("Expected a geometry/geography EWKB wrapper"));
        }
        let bytes = STANDARD.decode(&self.value)
            .map_err(|_| SpatialError::geometry("Invalid EWKB base64"))?;
        let info = inspect(&bytes)?;
        if self.srid != info.srid || self.dimensions != info.dimensions
            || self.is_empty != (info.positions == 0) {
            return Err(SpatialError::geometry("EWKB wrapper metadata does not match its bytes"));
        }
        Ok((bytes, info))
    }
}

pub fn base_type(mut ty: &Type) -> &Type {
    while let Kind::Domain(base) = ty.kind() { ty = base; }
    ty
}

pub fn native_type(ty: &Type) -> Option<&str> {
    let base = base_type(ty);
    (matches!(base.kind(), Kind::Simple)
        && base.oid() >= 16_384
        && matches!(base.name(), "geometry" | "geography"))
        .then(|| base.name())
}

pub fn is_spatial_array(ty: &Type) -> bool {
    match base_type(ty).kind() {
        Kind::Array(element) => native_type(element).is_some() || is_spatial_array(element),
        _ => false,
    }
}

pub struct SpatialBytes(pub Vec<u8>);

impl<'a> FromSql<'a> for SpatialBytes {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(raw.to_vec()))
    }
    fn accepts(ty: &Type) -> bool { native_type(ty).is_some() }
}

pub fn inspect(bytes: &[u8]) -> Result<EwkbInfo, SpatialError> {
    let mut cursor = Cursor { bytes, offset: 0 };
    let info = cursor.geometry(0)?;
    if cursor.offset != bytes.len() { return Err(SpatialError::geometry("Trailing EWKB bytes")); }
    Ok(info)
}

struct Cursor<'a> { bytes: &'a [u8], offset: usize }

impl Cursor<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], SpatialError> {
        let end = self.offset.checked_add(count).filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| SpatialError::geometry("Truncated EWKB"))?;
        let result = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(result)
    }
    fn integer(&mut self, little: bool) -> Result<u32, SpatialError> {
        let bytes: [u8; 4] = self.take(4)?.try_into().unwrap();
        Ok(if little { u32::from_le_bytes(bytes) } else { u32::from_be_bytes(bytes) })
    }
    fn count(&mut self, little: bool) -> Result<usize, SpatialError> {
        let n = self.integer(little)? as usize;
        if n > self.bytes.len().saturating_sub(self.offset) {
            return Err(SpatialError::geometry("Invalid EWKB element count"));
        }
        Ok(n)
    }
    fn point(&mut self, little: bool, dimensions: usize) -> Result<(usize,bool), SpatialError> {
        let mut xy = [0.0;2];
        for index in 0..dimensions {
            let bytes: [u8; 8] = self.take(8)?.try_into().unwrap();
            let v = if little { f64::from_le_bytes(bytes) } else { f64::from_be_bytes(bytes) };
            if let Some(coordinate)=xy.get_mut(index) { *coordinate=v; }
        }
        let empty=xy.iter().all(|value|value.is_nan());
        Ok((usize::from(!empty),!empty && !xy.iter().all(|value|value.is_finite())))
    }
    fn geometry(&mut self, depth: usize) -> Result<EwkbInfo, SpatialError> {
        if depth > 64 { return Err(SpatialError::geometry("EWKB nesting exceeds 64 levels")); }
        let little = match self.take(1)?[0] { 0 => false, 1 => true,
            _ => return Err(SpatialError::geometry("Invalid EWKB byte order")) };
        let word = self.integer(little)?;
        let mut geometry_type = word & 0x0fff_ffff;
        let iso = geometry_type / 1000;
        geometry_type %= 1000;
        let z = word & 0x8000_0000 != 0 || matches!(iso, 1 | 3);
        let m = word & 0x4000_0000 != 0 || matches!(iso, 2 | 3);
        let dimensions = match (z, m) { (false, false) => "XY", (true, false) => "XYZ",
            (false, true) => "XYM", (true, true) => "XYZM" };
        let dim = 2 + usize::from(z) + usize::from(m);
        let srid = if word & 0x2000_0000 != 0 {
            let srid = self.integer(little)? as i32;
            (srid != 0).then_some(srid)
        } else { None };
        let mut positions = 0usize;
        let mut invalid_xy = false;
        let mut supported = (1..=7).contains(&geometry_type);
        match geometry_type {
            1 => { let point=self.point(little,dim)?;positions=point.0;invalid_xy=point.1; },
            2 | 8 => {
                let n = self.count(little)?;
                for _ in 0..n { let point=self.point(little,dim)?;positions+=point.0;invalid_xy|=point.1; }
            }
            3 | 17 => {
                let n = self.count(little)?;
                for _ in 0..n {
                    let n = self.count(little)?;
                    for _ in 0..n { let point=self.point(little,dim)?;positions+=point.0;invalid_xy|=point.1; }
                }
            }
            4..=7 | 9..=12 | 15 | 16 => {
                let n = self.count(little)?;
                for _ in 0..n {
                    let child = self.geometry(depth + 1)?;
                    positions = positions.checked_add(child.positions)
                        .ok_or_else(|| SpatialError::geometry("EWKB coordinate count overflow"))?;
                    supported &= child.supported;
                    invalid_xy |= child.invalid_xy;
                }
            }
            _ => return Err(SpatialError::unsupported("Unknown EWKB geometry type")),
        }
        Ok(EwkbInfo { srid, dimensions, positions, supported, invalid_xy })
    }
}
