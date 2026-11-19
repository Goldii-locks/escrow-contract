# Event Reference

All events emitted by the `milestone-escrow` contract, keyed by their
`symbol_short!` topic. This table is kept in sync with the source by the
`event_docs_sync_tests` module — a topic cannot be added to `lib.rs` without a
row here, and a row here must correspond to a topic in the source.

## Indexed topics

`topic[0]` is always the event name below. These events also carry party
addresses as further topics, so RPC `getEvents` can filter by participant
(issue #739):

| Event      | Topics after the name                 |
| ---------- | ------------------------------------- |
| `init`     | `client`, `freelancer`, `arbiter`     |
| `fund`     | `client`, `freelancer`, `arbiter`     |
| `deliver`  | `freelancer`                          |
| `claim`    | `freelancer`                          |
| `approve`  | `client`                              |
| `dispute`  | `caller`                              |
| `freerot`  | `new_freelancer`                      |

## Events

| Topic      | Event struct                     | Kind              |
| ---------- | -------------------------------- | ----------------- |
| `init`     | `InitializedEvent`               | state change      |
| `fund`     | `FundedEvent`                    | state change      |
| `acceptjob`| `JobAcceptedEvent`               | state change      |
| `acceptarb`| `JobAcceptedEvent`               | state change      |
| `unaccref` | `UnacceptedWithdrawnEvent`       | state change      |
| `overdue`  | `OverdueReclaimedEvent`          | state change      |
| `withdraw` | `WithdrawnEvent`                 | state change      |
| `freerot`  | `FreelancerRotatedEvent`         | state change      |
| `arbchg`   | `ArbiterReplacedEvent`           | state change      |
| `rolechg`  | `AdminRoleChangedEvent`          | state change      |
| `msfund`   | `MilestoneFundedEvent`           | state change      |
| `ms_add`   | `MilestoneAddedEvent`            | state change      |
| `ms_reduce`| `MilestoneReducedEvent`          | state change      |
| `deliver`  | `DeliveredEvent`                 | state change      |
| `msrev`    | `RevisionRequestedEvent`         | state change      |
| `approve`  | `ApprovedEvent`                  | state change      |
| `claim`    | `ClaimedEvent`                   | state change      |
| `extend`   | `DeadlineExtendedEvent`          | state change      |
| `dispute`  | `DisputeRaisedEvent`             | state change      |
| `evidence` | `EvidenceSubmittedEvent`         | state change      |
| `dispescal`| `DisputeEscalatedEvent`          | state change      |
| `resolve`  | `DisputeResolvedEvent`           | state change      |
| `sweep`    | `ExcessSweptEvent`               | state change      |
| `cancel`   | `CancelEscrowInitiatedEvent`     | state change      |
| `cxlappr`  | `CancelApprovalRecordedEvent`    | state change      |
| `cancelrev`| `CancelApprovalRevokedEvent`     | state change      |
| `upgrade`  | `ContractUpgradedEvent`          | state change      |
| `migrate`  | `MigratedEvent`                  | state change      |
| `wtok`     | `TokenWhitelistedEvent`          | state change      |
| `wldel`    | `TokenRemovedEvent`              | state change      |
| `pause`    | `EscrowPausedEvent`              | state change      |
| `resume`   | `EscrowResumedEvent`             | state change      |
| `empause`  | `EmergencyPausedEvent`           | state change      |
| `emunpause`| `EmergencyUnpausedEvent`         | state change      |
| `emoverrid`| `EmergencyPauseAdminOverrideEvent` | state change    |
| `epalloc`  | `EmergencyPauseAllocationEvent`  | calculation       |
| `epclaim`  | `EmergencyPauseClaimRefundEvent` | state change      |
| `admin`    | `TransferAdminEvent`             | state change      |
| `adminprp` | `AdminTransferProposedEvent`     | state change      |
| `admincxl` | `AdminTransferCancelledEvent`    | state change      |
| `adminexc` | `AdminTransferExecutedEvent`     | state change      |
| `adcovls`  | `AdminCancelOverrideReleaseEvent`| state change      |
| `adcovrf`  | `AdminCancelOverrideRefundEvent` | state change      |
| `admovrls` | `AdminOverrideReleaseEvent`      | state change      |
| `admovrf`  | `AdminOverrideRefundEvent`       | state change      |
| `admstrm`  | `AdminOverrideStreamingReleaseEvent` | state change  |
| `adtxrls`  | `AdminOverrideTaxReleaseEvent`   | state change      |
| `adtxrfd`  | `AdminOverrideTaxRefundEvent`    | state change      |
| `adtwsplt` | `AdminOverrideTaxSplitRefundEvent` | calculation     |
| `arbsplit` | `ArbitrationSplitAppliedEvent`   | calculation       |
| `msiginit` | `MultisigApprovalInitEvent`      | state change      |
| `msigcfgp` | `MultiSigConfigProposedEvent`    | state change      |
| `msigcfg`  | `MultiSigConfigChangedEvent`     | state change      |
| `msigcfgx` | `MultiSigConfigCancelledEvent`   | state change      |
| `msigappr` | `MultiSigApprovedEvent`          | state change      |
| `mslock`   | `MultisigLockedEvent`            | state change      |
| `msadmrel` | `MultisigAdminOverrideReleaseEvent` | state change   |
| `msadmref` | `MultisigAdminOverrideRefundEvent`  | state change   |
| `intyield` | `EscrowInterestYieldEvent`       | calculation       |
| `yldset`   | `EscrowInterestYieldSetEvent`    | state change      |
| `yldcons`  | `EscrowInterestYieldConsentSetEvent` | state change  |
| `yldrate`  | `YieldRateSetEvent`              | state change      |
| `yldacc`   | `YieldAccruedEvent`              | calculation       |
| `yldlock`  | `EscrowInterestYieldLockedEvent` | state change      |
| `yldunlock`| `EscrowInterestYieldUnlockedEvent` | state change    |
| `pf_set`   | `PlatformFeeAllocationSetEvent`  | state change      |
| `pf_lock`  | `PlatformFeeAllocationLockedEvent` | state change    |
| `pf_ovr`   | `PlatformFeeAllocationOverrideEvent` | state change  |
| `pf_split` | `PlatformFeeSplitCalculatedEvent`| calculation       |
| `p_stream` | `PaymentStreamingEvent`          | calculation       |
| `p_strcns` | `PaymentStreamingConsentEvent`   | state change      |
| `m_ext`    | `MilestoneTimeExtensionEvent`    | state change      |
| `m_extcns` | `TimeExtConsentEvent`            | state change      |
| `taxwith`  | `TaxWithholdingAppliedEvent`     | state change      |
| `taxrcpt`  | `TaxRecipientSetEvent`           | state change      |
| `taxpaid`  | `TaxPaidEvent`                   | state change      |
| `taxwh`    | `TaxWithholdingDeductionsEvent`  | calculation       |
| `twspltref`| `TaxWithholdingSplitRefundEvent` | calculation       |
| `splitref` | `SplitRefundCalculatedEvent`     | calculation       |
| `iyspltref`| `SplitRefundCalculatedEvent`     | calculation       |
| `epspltref`| `SplitRefundCalculatedEvent`     | calculation       |
| `cxlspref` | `CancelSplitRefundCalculatedEvent` | calculation     |
| `sprefnet` | `SplitRefundNetDistributionEvent`| calculation       |

## Dispute records

`dispute` is emitted with `caller` as its topic and the same `caller` in its
data. The persistent `DisputeInfo` record that `get_dispute_info` returns also
carries `raised_by` (#997), the party that called `raise_dispute`, so the
raiser can be read from contract state without replaying events. For disputes
that were already open when an escrow migrated from schema 3 to 4, `raised_by`
is the job's client as a placeholder because the raiser was never stored; the
original `dispute` event's `caller` topic is authoritative for those. `migrate`
emits `migrate` with `from_version` 3 and `to_version` 4 when it stamps them.

## Notes

- **Kind** distinguishes events that record a durable state change from those
  that report a derived calculation (fee/tax/split math). Indexers should treat
  calculation events as advisory and reconcile against persisted state.
- Several split-refund paths share the `SplitRefundCalculatedEvent` struct but
  use distinct topics (`arbsplit`, `epspltref`, `iyspltref`, `splitref`) so
  consumers can tell the originating workflow apart.
- **Cancel approvals expire.** `cxlappr` carries `approved_at`, the ledger
  timestamp at which the approval round started. An approval older than
  `CANCEL_APPROVAL_TTL_SECONDS` (7 days) is treated as absent by
  `cancel_escrow`, so `cancel` only ever follows two approvals recorded within
  the same 7-day window — a stale `cxlappr` without a matching `cancel` should
  be treated as lapsed.

## Escrow factory events

The `escrow-factory` contract (`contracts/escrow-factory`) emits a single
event. It is documented separately because it comes from a different contract
than the topics above and is not covered by `event_docs_sync_tests`.

| Topic     | Event struct         | Kind         |
| --------- | -------------------- | ------------ |
| `created` | `EscrowCreatedEvent` | state change |

Emitted by `create_escrow` after the escrow has been deployed and its
constructor has run.

**Topics:** `created`, `client`, `freelancer`, `arbiter`. The party addresses
are indexed so RPC `getEvents` can filter by participant, as for the escrow's
`init` event.

**Data (`EscrowCreatedEvent`):**

| Field                  | Type             | Meaning                                                    |
| ---------------------- | ---------------- | ---------------------------------------------------------- |
| `escrow`               | `Address`        | The newly deployed escrow contract.                        |
| `token`                | `Address`        | Escrow token (must be on the factory allow-list).          |
| `total_amount`         | `i128`           | Sum of all milestone amounts, in the token's smallest unit.|
| `milestone_count`      | `u32`            | Number of milestones the escrow was created with.          |
| `auto_release_seconds` | `u64`            | Auto-release window passed to the escrow constructor.      |
| `settings`             | `EscrowSettings` | `funding_mode`, `dispute_timeout_seconds`, `arbiter_fee_bps` exactly as passed to the constructor. |
| `wasm_hash`            | `BytesN<32>`     | Escrow WASM hash the factory used for this deployment.     |

**Breaking change for indexers.** Before this change the event had one topic
(`created`) and data `{ escrow, client, freelancer, arbiter, token }`. The
party addresses moved from the data into topics 1 to 3, and `total_amount`,
`milestone_count`, `auto_release_seconds`, `settings` and `wasm_hash` were
added. Consumers that decoded `client`, `freelancer` or `arbiter` from the data
must read them from the topics instead. `wasm_hash` lets an indexer tell which
escrow build an address runs without calling the factory, which matters because
`set_wasm_hash` changes the hash for later escrows.
