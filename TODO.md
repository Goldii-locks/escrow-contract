# TODO

- [x] Inspect existing escrow contract logic around `approve_partial` and `claim_auto_release` deadline/status transitions.
- [ ] Add edge-case tests covering deadline boundary interactions:
  - [ ] Partial release exactly at deadline timestamp boundary, confirm final status/amount.
  - [ ] Client partially releases moments before deadline; freelancer `claim_auto_release` must fail.
  - [ ] `claim_auto_release` after a partial release must release only remaining unreleased amount.
- [ ] Run full test suite (`cargo test`) and ensure all tests pass and snapshots updated only if needed.
- [ ] Add code comments documenting any discovered edge-case behavior.


