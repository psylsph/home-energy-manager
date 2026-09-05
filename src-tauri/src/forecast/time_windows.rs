use chrono::{DateTime, Duration, LocalResult, NaiveDate, TimeZone, Utc};

/// Return the fraction of the absolute hour beginning at `timestamp` that is
/// covered by a local wall-clock window. The window may wrap midnight.
///
/// Converting both interval endpoints to epoch seconds is important for
/// half-hour-offset zones and DST transitions: a local hour can cross a date
/// boundary, skip an hour, or repeat an hour while the simulation bucket is
/// still exactly 3,600 seconds long.
pub(crate) fn window_overlap_hours<Tz: TimeZone>(
    timestamp: i64,
    start_min: u16,
    end_min: u16,
    timezone: &Tz,
) -> f64 {
    window_overlap_segments(timestamp, start_min, end_min, timezone)
        .iter()
        .map(|(start, end)| end - start)
        .sum()
}

/// Return the absolute-hour offsets covered by a local wall-clock window
/// inside the hour beginning at `timestamp`. Each pair is a half-open
/// interval in `[0.0, 1.0]` relative to the real 3,600-second bucket. The
/// intervals are absolute-time intersections, so they remain correct when a
/// local window crosses midnight or a DST transition.
pub(crate) fn window_overlap_segments<Tz: TimeZone>(
    timestamp: i64,
    start_min: u16,
    end_min: u16,
    timezone: &Tz,
) -> Vec<(f64, f64)> {
    let Some(hour_end) = timestamp.checked_add(3_600) else {
        return Vec::new();
    };
    let Some(hour_start_utc) = DateTime::<Utc>::from_timestamp(timestamp, 0) else {
        return Vec::new();
    };
    let local_date = hour_start_utc.with_timezone(timezone).date_naive();

    if start_min == end_min {
        return Vec::new();
    }

    let mut segments = Vec::new();
    for day_offset in -1..=1 {
        let Some(date) = local_date.checked_add_signed(Duration::days(day_offset)) else {
            continue;
        };
        let Some((window_start, window_end)) =
            local_window_interval(timezone, date, start_min, end_min)
        else {
            continue;
        };
        let start = timestamp.max(window_start);
        let end = hour_end.min(window_end);
        if end > start {
            segments.push((
                (start - timestamp) as f64 / 3_600.0,
                (end - timestamp) as f64 / 3_600.0,
            ));
        }
    }

    segments.sort_by(|a, b| a.0.total_cmp(&b.0));
    segments
}

/// Split an hourly bucket at the supplied active-window boundaries. The
/// returned flag identifies intervals whose midpoint is inside an active
/// segment. Endpoints are clamped because callers may combine absolute
/// intersections from more than one window.
pub(crate) fn split_hour_segments(active: &[(f64, f64)]) -> Vec<(f64, f64, bool)> {
    let mut boundaries = vec![0.0, 1.0];
    for &(start, end) in active {
        boundaries.push(start.clamp(0.0, 1.0));
        boundaries.push(end.clamp(0.0, 1.0));
    }
    boundaries.sort_by(f64::total_cmp);
    boundaries.dedup_by(|a, b| (*a - *b).abs() < 1e-9);

    boundaries
        .windows(2)
        .filter_map(|bounds| {
            let start = bounds[0];
            let end = bounds[1];
            (end > start).then(|| {
                let midpoint = (start + end) / 2.0;
                let inside = active.iter().any(|(active_start, active_end)| {
                    midpoint >= *active_start && midpoint < *active_end
                });
                (start, end, inside)
            })
        })
        .collect()
}

fn local_window_interval<Tz: TimeZone>(
    timezone: &Tz,
    start_date: NaiveDate,
    start_min: u16,
    end_min: u16,
) -> Option<(i64, i64)> {
    let end_date = if end_min < start_min {
        start_date.checked_add_signed(Duration::days(1))?
    } else {
        start_date
    };
    let start = local_boundary_timestamp(timezone, start_date, start_min, false)?;
    let end = local_boundary_timestamp(timezone, end_date, end_min, true)?;
    (end > start).then_some((start, end))
}

fn local_boundary_timestamp<Tz: TimeZone>(
    timezone: &Tz,
    mut date: NaiveDate,
    minute: u16,
    prefer_latest: bool,
) -> Option<i64> {
    if minute == 1440 {
        date = date.checked_add_signed(Duration::days(1))?;
    }
    let minute = minute % 1440;
    let naive = date.and_hms_opt(u32::from(minute / 60), u32::from(minute % 60), 0)?;
    match timezone.from_local_datetime(&naive) {
        LocalResult::Single(value) => Some(value.timestamp()),
        LocalResult::Ambiguous(earliest, latest) => Some(if prefer_latest {
            latest.timestamp()
        } else {
            earliest.timestamp()
        }),
        LocalResult::None => {
            // A spring-forward transition can remove the requested wall
            // clock minute. Resolve it to the first real instant at or after
            // that wall time (the transition boundary), rather than dropping
            // the whole window.
            let guess = naive.and_utc().timestamp();
            // The UTC interpretation of a wall time is not a useful lower
            // bound: positive offsets can make it resolve too late, while
            // negative offsets can put the transition many hours ahead. Scan
            // a broad interval around the guess and select the first real
            // instant whose local wall time reaches the requested minute.
            (-24 * 3_600..=24 * 3_600).step_by(60).find_map(|offset| {
                let candidate = guess.checked_add(i64::from(offset))?;
                let candidate_local =
                    DateTime::<Utc>::from_timestamp(candidate, 0)?.with_timezone(timezone);
                (candidate_local.date_naive() == date && candidate_local.naive_local() >= naive)
                    .then_some(candidate)
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Timelike, Utc};
    use chrono_tz::{America::New_York, Australia::Adelaide, Europe::London};

    use super::{local_boundary_timestamp, window_overlap_hours};

    #[test]
    fn overlap_handles_half_hour_bucket_crossing_local_midnight() {
        let timestamp = Adelaide
            .with_ymd_and_hms(2025, 6, 15, 23, 30, 0)
            .single()
            .unwrap()
            .timestamp();

        let overlap = window_overlap_hours(timestamp, 23 * 60, 60, &Adelaide);

        assert!((overlap - 1.0).abs() < f64::EPSILON, "overlap = {overlap}");
    }

    #[test]
    fn overlap_handles_spring_forward_bucket_in_absolute_time() {
        let timestamp = Utc
            .with_ymd_and_hms(2025, 3, 30, 0, 0, 0)
            .single()
            .unwrap()
            .timestamp();

        // In London this bucket is 00:00–02:00 local because 01:00 is
        // skipped. The 00:30–02:30 wall-clock window therefore overlaps half
        // of this real hour, not a full local-minute hour.
        let overlap = window_overlap_hours(timestamp, 30, 150, &London);

        assert!((overlap - 0.5).abs() < f64::EPSILON, "overlap = {overlap}");
    }

    #[test]
    fn skipped_wall_time_resolves_first_valid_instant_in_negative_offset_zone() {
        let date = chrono::NaiveDate::from_ymd_opt(2025, 3, 9).unwrap();
        let timestamp = local_boundary_timestamp(&New_York, date, 2 * 60 + 30, false)
            .expect("DST-gap wall time should resolve");
        let local = Utc
            .timestamp_opt(timestamp, 0)
            .single()
            .unwrap()
            .with_timezone(&New_York);
        assert_eq!(local.hour(), 3);
        assert_eq!(local.minute(), 0);
    }
}
