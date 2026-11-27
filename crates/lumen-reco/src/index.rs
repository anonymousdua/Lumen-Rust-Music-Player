//! In-memory index of every analyzed track's vector. Brute force: at 10,000
//! tracks a full pass takes well under a millisecond.

use std::collections::HashMap;

use lumen_core::TrackId;

use crate::features::{LOUDNESS, Metric, TEMPO, Vector};

pub struct AudioIndex {
    ids: Vec<TrackId>,
    vecs: Vec<Vector>,
    pos: HashMap<TrackId, usize>,
    metric: Metric,
}

impl AudioIndex {
    pub fn new(metric: Metric) -> Self {
        Self {
            ids: Vec::new(),
            vecs: Vec::new(),
            pos: HashMap::new(),
            metric,
        }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Adds or replaces a track's vector.
    pub fn insert(&mut self, id: TrackId, v: Vector) {
        match self.pos.get(&id) {
            Some(&i) => self.vecs[i] = v,
            None => {
                self.pos.insert(id, self.ids.len());
                self.ids.push(id);
                self.vecs.push(v);
            }
        }
    }

    pub fn get(&self, id: TrackId) -> Option<&Vector> {
        self.pos.get(&id).map(|&i| &self.vecs[i])
    }

    pub fn distance(&self, a: &Vector, b: &Vector) -> f32 {
        self.metric.distance(a, b)
    }

    /// Mean distance from each indexed track to `seeds`.
    pub fn distances_to<'a>(
        &'a self,
        seeds: &'a [Vector],
    ) -> impl Iterator<Item = (TrackId, f32)> + 'a {
        let n = seeds.len().max(1) as f32;
        self.ids.iter().zip(&self.vecs).map(move |(&id, v)| {
            let sum: f32 = seeds.iter().map(|s| self.metric.distance(s, v)).sum();
            (id, sum / n)
        })
    }

    /// The `k` nearest tracks to `seed` (closest first), without `seed`.
    pub fn nearest(&self, seed: TrackId, k: usize) -> Vec<(TrackId, f32)> {
        let Some(s) = self.get(seed).copied() else {
            return Vec::new();
        };
        let mut all: Vec<(TrackId, f32)> = self
            .distances_to(&[s])
            .filter(|(id, _)| *id != seed)
            .collect();
        all.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.0.cmp(&b.0.0)));
        all.truncate(k);
        all
    }

    /// How calm each track is, 0 (fast and loud) to 1 (slow and quiet):
    /// the average of its tempo and loudness percentiles, inverted.
    pub fn calmness(&self) -> HashMap<TrackId, f32> {
        let n = self.ids.len();
        if n == 0 {
            return HashMap::new();
        }
        let rank = |feature: usize| -> Vec<f32> {
            let mut order: Vec<usize> = (0..n).collect();
            order.sort_by(|&a, &b| {
                self.vecs[a][feature]
                    .total_cmp(&self.vecs[b][feature])
                    .then(self.ids[a].0.cmp(&self.ids[b].0))
            });
            let mut r = vec![0.0; n];
            let denom = (n - 1).max(1) as f32;
            for (k, &i) in order.iter().enumerate() {
                r[i] = k as f32 / denom;
            }
            r
        };
        let (tempo, loud) = (rank(TEMPO), rank(LOUDNESS));
        self.ids
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, 1.0 - (tempo[i] + loud[i]) / 2.0))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::DIM;

    fn v(x: f32) -> Vector {
        [x; DIM]
    }

    #[test]
    fn nearest_is_sorted_and_skips_the_seed() {
        let mut ix = AudioIndex::new(Metric::bliss().expect("metric"));
        for (id, x) in [(1, 0.0), (2, 0.5), (3, 0.1), (4, -0.3)] {
            ix.insert(TrackId(id), v(x));
        }
        let near: Vec<i64> = ix.nearest(TrackId(1), 2).iter().map(|n| n.0.0).collect();
        assert_eq!(near, [3, 4]);
        ix.insert(TrackId(3), v(0.9));
        assert_eq!(ix.len(), 4);
        assert_eq!(ix.nearest(TrackId(1), 1)[0].0, TrackId(4));
    }

    #[test]
    fn calmness_prefers_slow_and_quiet() {
        let mut ix = AudioIndex::new(Metric::bliss().expect("metric"));
        let mut calm = v(0.0);
        calm[TEMPO] = -0.9;
        calm[LOUDNESS] = -0.9;
        let mut loud = v(0.0);
        loud[TEMPO] = 0.9;
        loud[LOUDNESS] = 0.9;
        ix.insert(TrackId(1), calm);
        ix.insert(TrackId(2), loud);
        ix.insert(TrackId(3), v(0.0));
        let c = ix.calmness();
        assert!(c[&TrackId(1)] > c[&TrackId(3)]);
        assert!(c[&TrackId(3)] > c[&TrackId(2)]);
        assert_eq!(c[&TrackId(1)], 1.0);
        assert_eq!(c[&TrackId(2)], 0.0);
    }
}
