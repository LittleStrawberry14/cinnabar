use super::*;

/// Ordinary mesh retries have a fixed work budget and never revive superseded revisions.
#[test]
fn ordinary_mesh_promotion_is_bounded_and_current() {
    let view = SchedulerView {
        position: [8.0; 3],
        forward: Some([1.0, 0.0, 0.0]),
        startup_center: None,
    };
    let mut jobs = KeyedJobs::<PendingMesh, u64, 1>::default();
    jobs.refresh
        .refresh(view, &mut jobs.lanes, None, |_, _| true);
    let count = MAX_PENDING_SCHEDULER_SCANS_PER_POLL * 3;
    let now = Instant::now();
    for x in 0..=count {
        let key = SubChunkKey::new(1, x as i32, 0, 0);
        jobs.pending.insert(
            key,
            PendingMesh {
                revision: 2,
                since: now,
                queued_at: now,
                urgent: false,
            },
        );
        let candidate =
            PendingSchedulerCandidate::new(key, if x == 0 { 1 } else { 2 }, view, false);
        if x == count {
            jobs.lanes[0].ready.push(candidate);
        } else {
            jobs.lanes[0].deferred.push(candidate);
        }
    }
    jobs.ingress(view, None, |_, _, _| (0, false));
    let pending = &jobs.pending;
    let mut budget = MAX_PENDING_SCHEDULER_SCANS_PER_POLL;
    let candidates = jobs.lanes[0].take_deferred_rechecks(&mut budget, None, |key, revision| {
        pending
            .get(&key)
            .is_some_and(|job| job.revision == revision)
    });
    for candidate in candidates {
        jobs.lanes[0].record_deferred_recheck(candidate);
        jobs.lanes[0].ready.push(candidate);
    }
    let lane = &jobs.lanes[0];
    assert_eq!(
        lane.deferred.len(),
        count - MAX_PENDING_SCHEDULER_SCANS_PER_POLL
    );
    assert_eq!(lane.ready.len(), MAX_PENDING_SCHEDULER_SCANS_PER_POLL);
    assert!(lane.ready.iter().all(|candidate| candidate.revision == 2));
    let best = lane.ready.peek().unwrap().key;
    assert_eq!(best.x, 1);
    assert!(jobs.lanes[0].rechecks.contains_key(&best));
    jobs.remove_pending(&best);
    assert!(!jobs.lanes[0].rechecks.contains_key(&best));
    jobs.clear_queued();
    assert!(jobs.lanes[0].is_empty());
    assert!(jobs.lanes[0].rechecks.is_empty());
}

/// Cooling high-ranked retries cannot hide eligible work beyond one bounded scan window.
#[test]
fn cooling_retries_do_not_hide_deferred_work() {
    let view = SchedulerView {
        position: [8.0; 3],
        forward: None,
        startup_center: None,
    };
    let count = MAX_PENDING_SCHEDULER_SCANS_PER_POLL * 3;
    let eligible = SubChunkKey::new(0, count as i32, 0, 0);
    let mut lane = Lane::default();
    lane.ready.push(PendingSchedulerCandidate::new(
        SubChunkKey::new(0, eligible.x + 1, 0, 0),
        1,
        view,
        false,
    ));
    for x in 0..=count {
        let key = SubChunkKey::new(0, x as i32, 0, 0);
        lane.deferred
            .push(PendingSchedulerCandidate::new(key, 1, view, false));
        if key != eligible {
            lane.rechecks.insert(
                key,
                Recheck {
                    revision: 1,
                    attempts: 6,
                    not_before: u64::MAX,
                },
            );
        }
    }
    let mut offered = Vec::new();
    for _ in 0..=count / MAX_PENDING_SCHEDULER_SCANS_PER_POLL {
        let mut budget = MAX_PENDING_SCHEDULER_SCANS_PER_POLL;
        offered.extend(lane.take_deferred_rechecks(&mut budget, None, |_, _| true));
    }
    assert_eq!(
        offered
            .iter()
            .map(|candidate| candidate.key)
            .collect::<Vec<_>>(),
        [eligible]
    );
    assert_eq!(lane.rechecked.len(), count);
}

/// Unexamined retries stay eligible when the readiness slice ends after the first candidate.
#[test]
fn unexamined_deferred_rechecks_keep_their_eligibility() {
    let view = SchedulerView {
        position: [8.0; 3],
        forward: None,
        startup_center: None,
    };
    let mut lane = Lane::default();
    lane.ready.push(PendingSchedulerCandidate::new(
        SubChunkKey::new(0, 5, 0, 0),
        1,
        view,
        false,
    ));
    for x in 0..3 {
        let key = SubChunkKey::new(0, x, 0, 0);
        lane.deferred
            .push(PendingSchedulerCandidate::new(key, 1, view, false));
        lane.rechecks.insert(
            key,
            Recheck {
                revision: 1,
                attempts: 5,
                not_before: 0,
            },
        );
    }
    let mut budget = MAX_PENDING_SCHEDULER_SCANS_PER_POLL;
    let mut candidates = lane
        .take_deferred_rechecks(&mut budget, None, |_, _| true)
        .into_iter();
    // Only the first candidate reaches readiness; the rest return without an attempt.
    let checked = candidates.next().unwrap();
    lane.record_deferred_recheck(checked);
    lane.rechecked.push(checked);
    lane.deferred.extend(candidates);
    let mut budget = MAX_PENDING_SCHEDULER_SCANS_PER_POLL;
    let next = lane.take_deferred_rechecks(&mut budget, None, |_, _| true);
    assert_eq!(
        next.iter()
            .map(|candidate| candidate.key.x)
            .collect::<Vec<_>>(),
        [1, 2]
    );
}
