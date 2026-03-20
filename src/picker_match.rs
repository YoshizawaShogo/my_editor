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

    let display_name = candidate.display_name();
    let path = candidate.path().to_string_lossy();

    let display_score = score_text(display_name, query)?;
    let path_score = score_text(&path, query).unwrap_or(i64::MIN / 4);

    Some(base_candidate_score(candidate) + display_score.max(path_score))
}

fn base_candidate_score(candidate: &OpenCandidate) -> i64 {
    match candidate {
        OpenCandidate::OpenBuffer(_) => 1_000_000,
        OpenCandidate::ProjectFile(_) => 0,
    }
}

fn score_text(text: &str, query: &str) -> Option<i64> {
    let lower_text: Vec<char> = text.chars().flat_map(|c| c.to_lowercase()).collect();
    let lower_query: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();

    let mut score = 0i64;
    let mut query_index = 0usize;
    let mut last_match_index = None;

    for (text_index, text_char) in lower_text.iter().enumerate() {
        if query_index >= lower_query.len() {
            break;
        }

        if *text_char != lower_query[query_index] {
            continue;
        }

        score += 10;

        if text_index == 0 {
            score += 20;
        }

        if is_word_boundary(&lower_text, text_index) {
            score += 15;
        }

        if let Some(last_index) = last_match_index {
            if text_index == last_index + 1 {
                score += 25;
            }
        }

        last_match_index = Some(text_index);
        query_index += 1;
    }

    if query_index != lower_query.len() {
        return None;
    }

    score -= lower_text.len() as i64;
    Some(score)
}

fn is_word_boundary(text: &[char], index: usize) -> bool {
    if index == 0 {
        return true;
    }

    matches!(text[index - 1], '/' | '_' | '-' | ' ' | '.')
}
