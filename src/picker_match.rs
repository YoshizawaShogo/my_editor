use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};

use crate::open_candidate::OpenCandidate;

#[derive(Clone)]
pub struct PickerMatch {
    pub candidate: OpenCandidate,
    pub indices: Vec<usize>,
}

pub fn sort_open_candidates(candidates: &[OpenCandidate], query: &str) -> Vec<OpenCandidate> {
    ranked_open_candidates(candidates, query)
        .into_iter()
        .map(|matched| matched.candidate)
        .collect()
}

pub fn ranked_open_candidates(candidates: &[OpenCandidate], query: &str) -> Vec<PickerMatch> {
    let mut matches: Vec<(i64, &OpenCandidate, Vec<usize>)> = candidates
        .iter()
        .filter_map(|candidate| score_open_candidate(candidate, query))
        .collect();

    matches.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.display_name().cmp(right.1.display_name()))
            .then_with(|| left.1.path().cmp(right.1.path()))
    });

    matches
        .into_iter()
        .map(|(_, candidate, indices)| PickerMatch {
            candidate: candidate.clone(),
            indices,
        })
        .collect()
}

pub fn score_open_candidate<'a>(
    candidate: &'a OpenCandidate,
    query: &str,
) -> Option<(i64, &'a OpenCandidate, Vec<usize>)> {
    let query = query.trim();
    if query.is_empty() {
        return Some((base_candidate_score(candidate), candidate, Vec::new()));
    }

    let matcher = SkimMatcherV2::default();
    let display_name = candidate.display_name();
    let path = candidate.path().to_string_lossy();

    if let Some((score, indices)) = matcher.fuzzy_indices(display_name, query) {
        return Some((base_candidate_score(candidate) + score, candidate, indices));
    }

    let (score, _) = matcher.fuzzy_indices(&path, query)?;
    Some((base_candidate_score(candidate) + score, candidate, Vec::new()))
}

fn base_candidate_score(candidate: &OpenCandidate) -> i64 {
    match candidate {
        OpenCandidate::OpenBuffer(_) => 1_000_000,
        OpenCandidate::ProjectFile(_) => 0,
    }
}
