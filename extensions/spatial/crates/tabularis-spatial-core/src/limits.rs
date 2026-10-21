use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use crate::{CoreError, ErrorCode, Geometry, RowIdentity, MAX_SAFE_INTEGER};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeatureKind { Feature }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feature {
    #[serde(rename = "type")]
    pub kind: FeatureKind,
    pub id: String,
    #[serde(deserialize_with = "deserialize_geometry")]
    pub geometry: Option<Geometry>,
    pub properties: Map<String, Value>,
}

impl Feature {
    pub fn new(id: impl Into<String>, geometry: Option<Geometry>, properties: Map<String, Value>) -> Self {
        let geometry = geometry.filter(|geometry| !geometry.is_empty() || !matches!(geometry.coordinate_count(), Ok(0)));
        Self { kind: FeatureKind::Feature, id: id.into(), geometry, properties }
    }

    pub fn from_json(value: &Value) -> Result<Self, CoreError> {
        let geometry = value.get("geometry").ok_or_else(|| invalid("Feature geometry is required"))?;
        Geometry::from_json(geometry)?;
        let feature: Self = serde_json::from_value(value.clone()).map_err(|_| invalid("Malformed GeoJSON feature"))?;
        feature.coordinate_count()?;
        Ok(feature)
    }

    pub fn coordinate_count(&self) -> Result<u64, CoreError> {
        if self.id.is_empty() { return Err(invalid("Feature ID must be nonempty")); }
        self.geometry.as_ref().map_or(Ok(0), Geometry::coordinate_count)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowReference {
    pub feature_id: String,
    pub identity: Option<RowIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    pub row_ordinal: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeMode { None, Native, Materialized }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageInfo {
    pub next_token: Option<String>,
    pub has_more: bool,
    pub resume_mode: ResumeMode,
}

impl Default for PageInfo {
    fn default() -> Self { Self { next_token: None, has_more: false, resume_mode: ResumeMode::None } }
}

impl PageInfo {
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.has_more != self.next_token.is_some() || self.next_token.as_ref().is_some_and(String::is_empty)
            || (self.has_more && self.resume_mode == ResumeMode::None) {
            return Err(invalid("Continuation requires a nonempty token and a resume mode"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitStatus { pub truncated: bool, pub reasons: Vec<String> }

impl LimitStatus {
    pub fn merge(&mut self, other: &Self) {
        self.truncated |= other.truncated;
        for reason in &other.reasons {
            if !self.reasons.contains(reason) { self.reasons.push(reason.clone()); }
        }
    }

    pub fn truncate(&mut self, reason: impl Into<String>) {
        self.truncated = true;
        let reason = reason.into();
        if !self.reasons.contains(&reason) { self.reasons.push(reason); }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureError { pub index: u64, pub code: String, pub message: String }

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeaturePage {
    pub features: Vec<Feature>,
    pub row_references: Vec<RowReference>,
    pub page: PageInfo,
    pub limits: LimitStatus,
    pub warnings: Vec<String>,
    pub feature_errors: Vec<FeatureError>,
}

impl FeaturePage {
    pub fn from_json(value: &Value) -> Result<Self, CoreError> {
        let features = value.get("features").and_then(Value::as_array).ok_or_else(|| invalid("Page features are required"))?;
        for feature in features { Feature::from_json(feature)?; }
        let page: Self = serde_json::from_value(value.clone()).map_err(|_| invalid("Malformed feature page"))?;
        page.validate()?;
        Ok(page)
    }

    pub fn validate(&self) -> Result<(), CoreError> {
        self.page.validate()?;
        if self.features.len() != self.row_references.len() { return Err(invalid("Each feature requires one row reference")); }
        let mut ids = std::collections::HashSet::new();
        for (feature, reference) in self.features.iter().zip(&self.row_references) {
            feature.coordinate_count()?;
            validate_reference(feature, reference)?;
            if !ids.insert(&feature.id) { return Err(invalid("Feature IDs must be unique within a page")); }
        }
        Ok(())
    }

    pub fn serialized_bytes(&self) -> Result<u64, CoreError> {
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("Page cannot be serialized"))?;
        u64::try_from(bytes.len()).map_err(|_| overflow())
    }

    pub fn coordinate_count(&self) -> Result<u64, CoreError> {
        self.features.iter().try_fold(0_u64, |count, feature| add(count, feature.coordinate_count()?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageLimits { pub max_features: u64, pub max_coordinates: u64, pub max_bytes: u64 }

impl Default for PageLimits {
    fn default() -> Self { Self { max_features: 1_000, max_coordinates: 100_000, max_bytes: 8 * 1024 * 1024 } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerLimits { pub max_features: u64, pub max_coordinates: u64, pub max_bytes: u64 }

impl Default for LayerLimits {
    fn default() -> Self { Self { max_features: 10_000, max_coordinates: 250_000, max_bytes: 32 * 1024 * 1024 } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LayerBudget { pub features: u64, pub coordinates: u64, pub bytes: u64 }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushResult { Added, PageFull, LayerFull }

pub struct PageBuilder {
    page: FeaturePage,
    page_limits: PageLimits,
    layer_limits: LayerLimits,
    prior: LayerBudget,
    coordinates: u64,
    page_bytes: u64,
    layer_exhausted: bool,
}

impl PageBuilder {
    pub fn new(page_limits: PageLimits, layer_limits: LayerLimits, prior: LayerBudget) -> Result<Self, CoreError> {
        if page_limits.max_features == 0 || page_limits.max_coordinates == 0 || page_limits.max_bytes == 0
            || layer_limits.max_features == 0 || layer_limits.max_coordinates == 0 || layer_limits.max_bytes == 0 {
            return Err(invalid("Limits must be positive"));
        }
        let page = FeaturePage::default();
        let page_bytes = page.serialized_bytes()?;
        Ok(Self { page, page_limits, layer_limits, prior, coordinates: 0, page_bytes, layer_exhausted: false })
    }

    pub fn page(&self) -> &FeaturePage { &self.page }

    pub fn set_metadata(&mut self, page_info: PageInfo, upstream: LimitStatus, warnings: Vec<String>, feature_errors: Vec<FeatureError>) -> Result<(), CoreError> {
        page_info.validate()?;
        if self.layer_exhausted && page_info.has_more { return Err(invalid("A truncated layer cannot continue")); }
        let mut candidate = self.page.clone();
        candidate.page = page_info;
        candidate.limits.merge(&upstream);
        candidate.warnings = warnings;
        candidate.feature_errors = feature_errors;
        let bytes = candidate.serialized_bytes()?;
        if bytes > self.page_limits.max_bytes || add(self.prior.bytes, bytes)? > self.layer_limits.max_bytes {
            return Err(CoreError::new(ErrorCode::ResourceLimit, "Page metadata exceeds the byte limit"));
        }
        self.page = candidate;
        self.page_bytes = bytes;
        Ok(())
    }

    pub fn try_push(&mut self, feature: Feature, reference: RowReference) -> Result<PushResult, CoreError> {
        if self.layer_exhausted { return Ok(PushResult::LayerFull); }
        let coordinates = add(self.coordinates, feature.coordinate_count()?)?;
        validate_reference(&feature, &reference)?;
        if self.page.features.iter().any(|existing| existing.id == feature.id) { return Err(invalid("Duplicate feature ID")); }
        let features = add(u64::try_from(self.page.features.len()).map_err(|_| overflow())?, 1)?;
        let feature_bytes = serialized_length(&feature)?;
        let reference_bytes = serialized_length(&reference)?;
        let separators = if self.page.features.is_empty() { 0 } else { 2 };
        let bytes = add(self.page_bytes, add(add(feature_bytes, reference_bytes)?, separators)?)?;
        let exceeds_page = features > self.page_limits.max_features || coordinates > self.page_limits.max_coordinates || bytes > self.page_limits.max_bytes;
        if exceeds_page && self.page.features.is_empty() {
            return Err(CoreError::new(ErrorCode::FeatureTooLarge, "A complete feature and its reference exceed the page limit"));
        }
        let exceeds_layer = add(self.prior.features, features)? > self.layer_limits.max_features
            || add(self.prior.coordinates, coordinates)? > self.layer_limits.max_coordinates
            || add(self.prior.bytes, bytes)? > self.layer_limits.max_bytes;
        if exceeds_layer {
            self.mark_layer_full()?;
            return Ok(PushResult::LayerFull);
        }
        if exceeds_page { return Ok(PushResult::PageFull); }
        self.page.features.push(feature);
        self.page.row_references.push(reference);
        self.coordinates = coordinates;
        self.page_bytes = bytes;
        Ok(PushResult::Added)
    }

    fn mark_layer_full(&mut self) -> Result<(), CoreError> {
        let mut candidate = self.page.clone();
        candidate.limits.truncate("LAYER_LIMIT");
        candidate.page = PageInfo::default();
        loop {
            let bytes = candidate.serialized_bytes()?;
            if bytes <= self.page_limits.max_bytes && add(self.prior.bytes, bytes)? <= self.layer_limits.max_bytes { break; }
            if candidate.features.pop().is_none() { return Err(CoreError::new(ErrorCode::ResourceLimit, "The remaining byte budget cannot hold a truncated page envelope")); }
            candidate.row_references.pop();
        }
        self.coordinates = candidate.coordinate_count()?;
        self.page_bytes = candidate.serialized_bytes()?;
        self.page = candidate;
        self.layer_exhausted = true;
        Ok(())
    }

    pub fn finish(self) -> Result<(FeaturePage, LayerBudget), CoreError> {
        self.page.validate()?;
        let budget = LayerBudget {
            features: add(self.prior.features, u64::try_from(self.page.features.len()).map_err(|_| overflow())?)?,
            coordinates: add(self.prior.coordinates, self.coordinates)?,
            bytes: add(self.prior.bytes, self.page.serialized_bytes()?)?,
        };
        if budget.features > self.layer_limits.max_features || budget.coordinates > self.layer_limits.max_coordinates
            || budget.bytes > self.layer_limits.max_bytes || self.page.serialized_bytes()? > self.page_limits.max_bytes {
            return Err(CoreError::new(ErrorCode::ResourceLimit, "Page exceeds its remaining budget"));
        }
        Ok((self.page, budget))
    }
}

fn validate_reference(feature: &Feature, reference: &RowReference) -> Result<(), CoreError> {
    if reference.feature_id != feature.id || reference.row_ordinal > MAX_SAFE_INTEGER || reference.snapshot_id.as_ref().is_some_and(String::is_empty) {
        return Err(invalid("Row reference does not match its feature"));
    }
    if let Some(identity) = &reference.identity { identity.validate()?; }
    Ok(())
}

fn deserialize_geometry<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Geometry>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Geometry::from_json(&value).map_err(serde::de::Error::custom)
}

fn serialized_length(value: &impl Serialize) -> Result<u64, CoreError> {
    let bytes = serde_json::to_vec(value).map_err(|_| invalid("Feature cannot be serialized"))?;
    u64::try_from(bytes.len()).map_err(|_| overflow())
}

fn add(left: u64, right: u64) -> Result<u64, CoreError> { left.checked_add(right).ok_or_else(overflow) }
fn overflow() -> CoreError { CoreError::new(ErrorCode::ResourceLimit, "Budget counter overflow") }
fn invalid(message: &str) -> CoreError { CoreError::new(ErrorCode::InvalidArgument, message) }
