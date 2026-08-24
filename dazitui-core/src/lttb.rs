//! Largest-Triangle-Three-Buckets (LTTB) 降采样算法
pub fn lttb_downsample(data: &[(f64, f64)], threshold: usize) -> Vec<(f64, f64)> {
    let len = data.len();
    if threshold >= len || threshold <= 2 {
        return data.iter().take(threshold).copied().collect();
    }

    let mut sampled = Vec::with_capacity(threshold);
    sampled.push(data[0]);

    let bucket_size = (len - 2) as f64 / (threshold - 2) as f64;
    let mut a_idx = 0;

    for i in 0..(threshold - 2) {
        let bucket_start = ((i as f64 * bucket_size) as usize) + 1;
        let bucket_end = ((((i + 1) as f64 * bucket_size) as usize) + 1).min(len - 1);

        let next_bucket_start = ((((i + 1) as f64 * bucket_size) as usize) + 1).min(len - 1);
        let next_bucket_end = ((((i + 2) as f64 * bucket_size) as usize) + 1).min(len);

        let mut avg_c_x = 0.0;
        let mut avg_c_y = 0.0;
        let next_count = (next_bucket_end - next_bucket_start) as f64;
        if next_count > 0.0 {
            for p in &data[next_bucket_start..next_bucket_end] {
                avg_c_x += p.0;
                avg_c_y += p.1;
            }
            avg_c_x /= next_count;
            avg_c_y /= next_count;
        } else {
            avg_c_x = data[len - 1].0;
            avg_c_y = data[len - 1].1;
        }

        let p_a = data[a_idx];
        let mut max_area = -1.0;
        let mut max_idx = bucket_start;

        for (offset, p_b) in data[bucket_start..bucket_end].iter().enumerate() {
            let area = ((p_a.0 * (p_b.1 - avg_c_y)
                + p_b.0 * (avg_c_y - p_a.1)
                + avg_c_x * (p_a.1 - p_b.1))
                .abs())
                * 0.5;

            if area > max_area {
                max_area = area;
                max_idx = bucket_start + offset;
            }
        }

        sampled.push(data[max_idx]);
        a_idx = max_idx;
    }

    sampled.push(data[len - 1]);
    sampled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lttb_empty_and_small_data() {
        assert_eq!(lttb_downsample(&[], 10), vec![]);
        let small = vec![(0.0, 10.0), (1.0, 20.0)];
        assert_eq!(lttb_downsample(&small, 5), small);
        assert_eq!(lttb_downsample(&small, 2), small);
    }

    #[test]
    fn test_lttb_preserves_peak() {
        let mut data = Vec::new();
        for i in 0..100 {
            let y = if i == 50 { 500.0 } else { (i % 10) as f64 };
            data.push((i as f64, y));
        }

        let sampled = lttb_downsample(&data, 10);
        assert_eq!(sampled.len(), 10);
        assert_eq!(sampled.first().unwrap().0, 0.0);
        assert_eq!(sampled.last().unwrap().0, 99.0);

        let has_peak = sampled.iter().any(|p| p.0 == 50.0 && p.1 == 500.0);
        assert!(has_peak);
    }
}
