# PLAN — clt (work top to bottom, one issue per worker session)

Worker loop: top unchecked item only, fix + resolving test, commit, check off, stop. Routing: see AI_ISSUE_ROUTING.md.

## Items

- [ ] #1 journal rotate race (concurrent runs lose history). (Medium — do first)
- [ ] #2 watermark advances on lookup failure. (Low)
- [ ] #4 MCP read lock+rewrite (half-holds — verify, narrow). (Low)
- [ ] #3 scan reads then discards. (Low)
