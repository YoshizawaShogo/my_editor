use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};

use crate::open_candidate::OpenCandidate;

pub fn sort_open_candidates(candidates: &[OpenCandidate], query: &str) -> Vec<OpenCandidate> {
    let mut matches: Vec<(i64, &OpenCandidate)> = candidates
        .iter()
        .filter_map(|candidate| score_open_candidate(candidate, query).map(|score| (score, candidate)))
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
        .map(|(_, candidate)| candidate.clone())
        .collect()
}

pub fn score_open_candidate(candidate: &OpenCandidate, query: &str) -> Option<i64> {
    let query = query.trim();
    if query.is_empty() {
        return Some(base_candidate_score(candidate));
    }

    let matcher = SkimMatcherV2::default();
    let display_name = candidate.display_name();
    let path = candidate.path().to_string_lossy();

    let display_score = matcher.fuzzy_match(display_name, query);
    let path_score = matcher.fuzzy_match(&path, query);
    let score = display_score.or(path_score)?;

    Some(base_candidate_score(candidate) + score)
}

fn base_candidate_score(candidate: &OpenCandidate) -> i64 {
    match candidate {
        OpenCandidate::OpenBuffer(_) => 1_000_000,
        OpenCandidate::ProjectFile(_) => 0,
    }
}
