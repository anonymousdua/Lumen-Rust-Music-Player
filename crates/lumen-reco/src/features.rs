//! bliss-rs feature vectors: storage format and the distance between them.

use bliss_audio::{AnalysisIndex, FeaturesVersion, NUMBER_FEATURES};

/// Features per track in the current bliss version.
pub const DIM: usize = 23;
const _: () = assert!(NUMBER_FEATURES == DIM, "bliss changed its feature count");

/// One track's analysis, in bliss's feature order.
pub type Vector = [f32; DIM];

/// Position of the tempo feature.
pub const TEMPO: usize = AnalysisIndex::Tempo as usize;
/// Position of the mean loudness feature.
pub const LOUDNESS: usize = AnalysisIndex::MeanLoudness as usize;

/// The bliss feature version vectors are analyzed with. Stored with each
/// vector; a different version means the track is analyzed again.
pub fn features_version() -> u16 {
    u16::from(FeaturesVersion::LATEST)
}

/// The distance bliss recommends for its current features: a weighted
/// (Mahalanobis) distance with bliss's own weight matrix.
///
/// bliss computes it with `ndarray` (two allocations and a 23×23 matrix
/// product per pair, about 3 ms for 10,000 tracks). For feature version 2
/// the matrix is diagonal, so this keeps just the diagonal and computes
/// exactly the same value over flat arrays, about 40 times faster. The
/// `matches_bliss` test checks the two agree.
#[derive(Clone, Debug)]
pub struct Metric {
    weights: Vector,
}

impl Metric {
    /// bliss's weights for [`FeaturesVersion::LATEST`]. Fails if bliss ever
    /// ships a non-diagonal matrix (then this shortcut would be wrong).
    pub fn bliss() -> Result<Self, String> {
        let m = FeaturesVersion::LATEST.feature_weights();
        let mut weights = [0.0; DIM];
        for (i, row) in m.outer_iter().enumerate() {
            for (j, &v) in row.iter().enumerate() {
                if i == j {
                    weights[i] = v;
                } else if v != 0.0 {
                    return Err("bliss's distance weights are not diagonal".into());
                }
            }
        }
        Ok(Self { weights })
    }

    pub fn distance(&self, a: &Vector, b: &Vector) -> f32 {
        let mut sum = 0.0;
        for i in 0..DIM {
            let d = a[i] - b[i];
            sum += d * d * self.weights[i];
        }
        sum.sqrt()
    }
}

/// Vector → BLOB: 23 little-endian `f32`s.
pub fn to_blob(v: &Vector) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// BLOB → vector; `None` if the length is wrong or a value isn't finite.
pub fn from_blob(b: &[u8]) -> Option<Vector> {
    let (chunks, rest) = b.as_chunks::<4>();
    if chunks.len() != DIM || !rest.is_empty() {
        return None;
    }
    let mut v = [0.0; DIM];
    for (x, chunk) in v.iter_mut().zip(chunks) {
        *x = f32::from_le_bytes(*chunk);
        if !x.is_finite() {
            return None;
        }
    }
    Some(v)
}

/// A bliss analysis as a vector.
pub fn from_analysis(a: &bliss_audio::Analysis) -> Option<Vector> {
    let v = a.as_vec();
    let v: Vector = v.try_into().ok()?;
    v.iter().all(|x| x.is_finite()).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bliss_audio::Analysis;

    fn pseudo(seed: u32) -> Vector {
        let mut s = seed.wrapping_mul(2_654_435_761).max(1);
        let mut v = [0.0; DIM];
        for x in &mut v {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            *x = (s % 2000) as f32 / 1000.0 - 1.0;
        }
        v
    }

    #[test]
    fn matches_bliss() {
        let m = Metric::bliss().expect("diagonal weights");
        for k in 0..50 {
            let (a, b) = (pseudo(k), pseudo(k + 1000));
            let ours = m.distance(&a, &b);
            let fv = FeaturesVersion::LATEST;
            let theirs = Analysis::new(a.to_vec(), fv)
                .expect("a")
                .distance(&Analysis::new(b.to_vec(), fv).expect("b"));
            assert!(
                (ours - theirs).abs() <= 1e-4 * theirs.max(1.0),
                "{ours} vs {theirs}"
            );
        }
    }

    #[test]
    fn blob_round_trip() {
        let v = pseudo(7);
        assert_eq!(from_blob(&to_blob(&v)), Some(v));
        assert_eq!(from_blob(&[0u8; 10]), None);
        let mut bad = to_blob(&v);
        bad[0..4].copy_from_slice(&f32::NAN.to_le_bytes());
        assert_eq!(from_blob(&bad), None);
    }

    #[test]
    fn feature_positions() {
        assert_eq!(TEMPO, 0);
        assert_eq!(LOUDNESS, 8);
        assert_eq!(features_version(), 2);
    }
}
