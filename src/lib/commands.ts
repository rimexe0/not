export function commandMatches(label: string, keywords: string, query: string): boolean {
  const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const haystack = `${label} ${keywords}`.toLowerCase();
  return terms.every((term) => haystack.includes(term));
}
