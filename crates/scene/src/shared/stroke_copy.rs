use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::media::{AssetHash, Credit};
use super::{Edit, Error, Result, SharedStroke};
use crate::{CanvasId, OwnerId, RegionKey, Stroke, StrokeId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrokeProvenance {
    pub canvas: CanvasId,
    pub stroke: StrokeId,
    /// Immutable creation revision of the immediate source stroke.
    pub source_revision: u64,
    pub artifact_hash: AssetHash,
    /// Original artist credit, inherited unchanged through later copies.
    pub author: Credit,
    pub offset: [f64; 2],
    pub scale: f64,
}
impl StrokeProvenance {
    pub fn validate(&self) -> Result<()> {
        super::valid_revision(self.source_revision)?;
        if self.source_revision == 0
            || !self.scale.is_finite()
            || self.scale <= 0.0
            || self.scale > 1_000_000.0
            || self.offset.iter().any(|value| !value.is_finite() || value.abs() > 8192.0)
        {
            return Err(Error::Invalid("stroke copy transform or source revision"));
        }
        self.author.validate()
    }
}

#[derive(Debug, Clone)]
pub struct StrokeCopyTarget {
    pub canvas: CanvasId,
    pub id: StrokeId,
    pub owner: OwnerId,
    pub anchor: RegionKey,
    pub offset: [f64; 2],
    pub scale: f64,
}

/// Shared deterministic geometry/provenance construction. The server still
/// proves both current grants and supplies the original artist's trusted credit.
pub fn stroke_copy(source: &SharedStroke, source_canvas: CanvasId, source_credit: Credit, target: StrokeCopyTarget) -> Result<Edit> {
    source.stroke.validate()?;
    if !source.visible || source.stroke.canvas != source_canvas || source.created_revision == 0 || source.stroke.id == target.id {
        return Err(Error::Invalid("stroke copy source or new identity"));
    }
    let author = if let Some(provenance) = &source.provenance {
        provenance.validate()?;
        provenance.author.clone()
    } else {
        if source_credit.profile != source.stroke.owner {
            return Err(Error::Invalid("stroke copy artist"));
        }
        source_credit
    };
    let bytes = serde_json::to_vec(&source.stroke).map_err(|_| Error::Invalid("stroke copy encoding"))?;
    let provenance = StrokeProvenance {
        canvas: source_canvas,
        stroke: source.stroke.id,
        source_revision: source.created_revision,
        artifact_hash: AssetHash::new(&format!("{:x}", Sha256::digest(bytes)))?,
        author,
        offset: target.offset,
        scale: target.scale,
    };
    provenance.validate()?;
    let stroke = Stroke {
        id: target.id,
        canvas: target.canvas,
        owner: target.owner,
        revision: 0,
        anchor: target.anchor,
        points: source.stroke.points.iter().map(|point| [point[0] * target.scale + target.offset[0], point[1] * target.scale + target.offset[1]]).collect(),
        width: source.stroke.width * target.scale,
        color: source.stroke.color,
        erase: source.stroke.erase,
        brush: source.stroke.brush.clone(),
        samples: source.stroke.samples.clone(),
    };
    stroke.validate()?;
    crate::render::spatial_cells(&stroke, crate::MAX_SPATIAL_CELLS)?;
    Ok(Edit::CopyStroke { stroke, provenance: Box::new(provenance) })
}
