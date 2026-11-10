# Decisions

One line per decision: date, decision, reason.

| Date | Decision | Reason |
| --- | --- | --- |
| 2026-11-09 | `docs/spec.md` is the Markdown export of the source Claude Docs document of revision 3 (rev 21, 29,053 bytes), not the `script#spec-md` block of the HTML artifact. | The block sits in an iframe on another domain, unreadable from the session; the only expected difference: the date and author line, rendered as `Nov 9, 2026 · @Sonny`. |
