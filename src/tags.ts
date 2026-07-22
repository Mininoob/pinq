// Tags are stored note state; the editor extracts completed "#tag " tokens out
// of the text and into tags. This module holds the shared pieces.

/** Completed hashtag: word boundary before, whitespace after (still-typing tags
 *  at the end of the text are left alone until terminated). */
export const TAG_COMPLETE = /(^|\s)#([\p{L}\p{N}_-]+)(?=\s)/gu;

/** Tag → note count across tag lists, sorted by count desc then name. */
export function tallyTags(lists: string[][]): Array<{ tag: string; count: number }> {
  const counts = new Map<string, number>();
  for (const tags of lists) {
    for (const tag of tags) {
      counts.set(tag, (counts.get(tag) ?? 0) + 1);
    }
  }
  return [...counts.entries()]
    .map(([tag, count]) => ({ tag, count }))
    .sort((a, b) => b.count - a.count || a.tag.localeCompare(b.tag));
}
