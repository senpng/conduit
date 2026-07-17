/**
 * Minimal subsequence fuzzy matcher for the command palette.
 * No external deps (local-first); scores higher for consecutive runs,
 * word-start hits, and shorter candidates.
 */

export interface FuzzyMatch {
  score: number;
  /** Indices of matched chars in the candidate (for highlight). */
  indices: number[];
}

/**
 * Subsequence match of `query` in `candidate` (case-insensitive).
 * Returns null when query is not a subsequence.
 */
export function fuzzyMatch(query: string, candidate: string): FuzzyMatch | null {
  const q = query.toLowerCase();
  const c = candidate.toLowerCase();
  if (!q) return { score: 0, indices: [] };

  const indices: number[] = [];
  let score = 0;
  let qi = 0;
  let run = 0;
  for (let i = 0; i < c.length && qi < q.length; i++) {
    if (c[i] === q[qi]) {
      indices.push(i);
      run += 1;
      score += 1 + run * 4; // consecutive bonus dominates
      const prev = i === 0 ? " " : c[i - 1];
      if (prev === " " || prev === "-" || prev === "_" || prev === ":" || prev === "/") {
        score += 6; // word-start bonus
      }
      qi += 1;
    } else {
      run = 0;
    }
  }
  if (qi < q.length) return null;
  score -= candidate.length * 0.05; // prefer shorter candidates
  return { score, indices };
}

export interface FuzzyItem<T> {
  item: T;
  score: number;
  indices: number[];
}

/** Filter + rank `items` by `query`; stable order preserved within ties. */
export function fuzzyFilter<T>(
  query: string,
  items: T[],
  text: (item: T) => string,
  limit = 50,
): FuzzyItem<T>[] {
  if (!query.trim()) {
    return items.slice(0, limit).map((item) => ({ item, score: 0, indices: [] }));
  }
  const out: FuzzyItem<T>[] = [];
  items.forEach((item, i) => {
    const m = fuzzyMatch(query, text(item));
    if (m) out.push({ item, score: m.score, indices: m.indices });
    void i;
  });
  out.sort((a, b) => b.score - a.score);
  return out.slice(0, limit);
}
