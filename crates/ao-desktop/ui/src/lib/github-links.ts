export function buildCompareUrl(owner: string, repo: string, base: string, head: string): string {
  return `https://github.com/${owner}/${repo}/compare/${base}...${head}`;
}
