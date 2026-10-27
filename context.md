# Project Context

## June 13, 2026
### ✅ Completed (Frontend):
1. **Frontend Setup** - Created a Next.js + Tailwind CSS frontend in `escrow-frontend/`
2. **Contract Utility Functions** - Added `app/lib/contract.ts` with Soroban RPC integration
3. **Wallet Integration** - Built `app/context/WalletContext.tsx` using Freighter browser extension API
4. **Navbar Component** - Implemented `app/components/Navbar.tsx` with wallet connect/disconnect and links to Dashboard/Create Job
5. **Home Page** - Updated `app/page.tsx` with landing content and call-to-action
6. **Create Job Page** - Added `app/create/page.tsx` with form to create jobs with milestones
7. **MilestoneCard Component** - Created `app/components/MilestoneCard.tsx` with status badges and action buttons
8. **Job Dashboard Page** - Built `app/dashboard/page.tsx` with mock job data and milestone interaction
9. **Dev Server** - Successfully running on http://localhost:3001 with all routes compiled!

### ✅ Completed (Contract):
10. **Contract Deployment** - Deployed milestone escrow contract to Stellar Testnet!
    - Contract ID: `CDD5WKK3WT3QVKXMXTJNDIXE4T73FK6GGXDSD6UTJAH6YYZU52SQ4MUH`
    - Explorer: https://stellar.expert/explorer/testnet/contract/CDD5WKK3WT3QVKXMXTJNDIXE4T73FK6GGXDSD6UTJAH6YYZU52SQ4MUH

### ✅ Completed (Backend):
11. **Backend Setup** - Created an Express + TypeScript backend in `escrow-backend/`
12. **API Endpoints** - Added endpoints to get job state, build transactions, and submit signed transactions
13. **Pushed to GitHub** - Backend repo is live at https://github.com/Goldii-locks/escrow-backend

## June 15, 2026
### ✅ Activity Update
- **Contract**: Added event emission to all functions
- **Backend**: Added vec type handling for build-tx endpoint
- **Frontend**: Wired Create Job form to backend
- All changes pushed to GitHub!

## June 16, 2026
### ✅ Activity Update
- **Contract**: Added CONTRIBUTING.md file with contributor guidelines
- **Frontend**: Added loading skeleton to dashboard for better UX
- **Backend**: Added GET /api/jobs/by-wallet/:address endpoint (closes issue #1)
- **Fix**: Fixed type error in dashboard useState
- All changes pushed to GitHub!

## June 17, 2026
### ✅ Activity Update (Major Progress!)
- **Contract**: Added 10+ edge case tests (closes issue #3) - now total 15 tests!
  - Tests for invalid milestone index, wrong status, unauthorized access, etc.
- **Frontend**: Added CONTRIBUTING.md; wired dashboard to fetch real job data via backend
- **Backend**: Added CONTRIBUTING.md; updated job endpoints to parse contract response; added Jest integration test setup (closes issue #3)
- All changes committed and pushed!

## June 18, 2026
### ✅ Partial/Installment Milestone Releases (Major Feature!)
- **Contract**: Added `released_amount` field to Milestone struct; added `approve_partial` function; updated all existing functions to handle partial releases; added comprehensive tests
- **All tests passed!** (19 total tests)
- **Committed and pushed!**

## June 20, 2026
### ✅ Time-Locked Auto-Release (Major Feature!)
- **Contract**: Added `delivered_at: u64` to Milestone struct; added `auto_release_seconds: u64` to Job struct; updated `initialize()` signature to accept `auto_release_seconds`; updated `mark_delivered()` to set `delivered_at`; added `claim_auto_release()` public function; added `time_until_auto_release()` public function; added `DeadlineNotPassed` error variant; fixed all token client calls to use &Address; added 5+ new tests (total tests 32)
- **All tests passed!** ✅
- **Frontend**: Updated Create Job form to:
  - Add "Response Deadline (days)" input
  - Pass correct args to `initialize()` (admin, client, freelancer, arbiter, token, auto_release_seconds, milestone_amounts)
  - Fixed BigInt compatibility issue
- **Backend**: Updated build-tx endpoint to handle `u64` type (for auto_release_seconds)
- **All repos committed and pushed!**

### 📁 Updated Project Structure:
```
Milesto/
├── escrow-contract/            # Soroban smart contract
│   ├── Cargo.toml
│   ├── Cargo.lock
│   ├── .gitignore
│   ├── README.md
│   ├── CONTRIBUTING.md
│   ├── context.md
│   └── contracts/
│       └── milestone-escrow/
│           ├── Cargo.toml
│           ├── src/
│           │   ├── lib.rs
│           │   └── test.rs
│           └── test_snapshots/

├── escrow-frontend/            # Next.js frontend
│   ├── package.json
│   ├── package-lock.json
│   ├── tsconfig.json
│   ├── next.config.ts
│   ├── tailwind.config.ts
│   ├── postcss.config.mjs
│   ├── .gitignore
│   ├── .env.local
│   ├── .env.local.example
│   ├── README.md
│   ├── CONTRIBUTING.md
│   ├── app/
│   │   ├── layout.tsx
│   │   ├── page.tsx
│   │   ├── globals.css
│   │   ├── lib/
│   │   │   └── contract.ts
│   │   ├── context/
│   │   │   └── WalletContext.tsx
│   │   ├── components/
│   │   │   ├── Navbar.tsx
│   │   │   ├── MilestoneCard.tsx
│   │   │   └── LoadingSkeleton.tsx
│   │   ├── create/
│   │   │   └── page.tsx
│   │   └── dashboard/
│   │       └── page.tsx
│   └── public/

└── escrow-backend/             # Express backend
    ├── package.json
    ├── package-lock.json
    ├── tsconfig.json
    ├── jest.config.ts
    ├── .gitignore
    ├── .env.example
    ├── .env
    ├── README.md
    ├── CONTRIBUTING.md
    ├── __tests__/
    │   └── jobs.test.ts
    └── src/
        ├── index.ts
        └── routes/
            └── jobs.ts
```

## June 22, 2026
### ✅ Backend Milestone Features & Event Indexer (Huge Progress!)
- **Issue #4 (Closed)**: Added GET /api/jobs/:contractId/whitelist endpoint to fetch whitelisted tokens with graceful NotInitialized error handling
- **Issue #5 (Closed)**: Updated build-tx endpoint to support admin whitelist management actions (add_whitelisted_token / remove_whitelisted_token) with validation
- **Issue #6 (Closed)**: Added POST /api/jobs/:contractId/milestones/:index/partial-release endpoint to build approve_partial transactions
- **Issue #7 (Closed)**: Added GET /api/jobs/:contractId/milestones/:index/time-remaining endpoint to fetch time left for auto-release
- **Issue #8 (Closed)**: Added POST /api/jobs/:contractId/milestones/:index/claim-auto-release endpoint to build claim_auto_release transactions
- **Event Indexer Service (New Feature!)**:
  - Added SQLite database integration (better-sqlite3 dependency)
  - Implemented db.ts with schema initialization, last_ledger tracking, event insertion (with UNIQUE constraint to avoid duplicates), and address-filtered querying
  - Implemented poller.ts that periodically fetches events from Soroban RPC, processes them, and stores in database
  - Updated index.ts to initialize indexer schema and start polling on server start
  - Added comprehensive tests for indexer database functionality
  - All 4 tests passed!
- **All TypeScript checks passed! npm run build is clean!**
- **All issues closed with manual comments (no auto-close keywords!)**
- **CI is green! All commits pushed to GitHub!**

### 📁 Updated Project Structure:
```
Milesto/
├── escrow-contract/            # Soroban smart contract
│   ├── Cargo.toml
│   ├── Cargo.lock
│   ├── .gitignore
│   ├── README.md
│   ├── CONTRIBUTING.md
│   ├── context.md
│   └── contracts/
│       └── milestone-escrow/
│           ├── Cargo.toml
│           ├── src/
│           │   ├── lib.rs
│           │   └── test.rs
│           └── test_snapshots/

├── escrow-frontend/            # Next.js frontend
│   ├── package.json
│   ├── package-lock.json
│   ├── tsconfig.json
│   ├── next.config.ts
│   ├── tailwind.config.ts
│   ├── postcss.config.mjs
│   ├── .gitignore
│   ├── .env.local
│   ├── .env.local.example
│   ├── README.md
│   ├── CONTRIBUTING.md
│   ├── app/
│   │   ├── layout.tsx
│   │   ├── page.tsx
│   │   ├── globals.css
│   │   ├── lib/
│   │   │   └── contract.ts
│   │   ├── context/
│   │   │   └── WalletContext.tsx
│   │   ├── components/
│   │   │   ├── Navbar.tsx
│   │   │   ├── MilestoneCard.tsx
│   │   │   └── LoadingSkeleton.tsx
│   │   ├── create/
│   │   │   └── page.tsx
│   │   └── dashboard/
│   │       └── page.tsx
│   └── public/

└── escrow-backend/             # Express backend
    ├── package.json
    ├── package-lock.json
    ├── tsconfig.json
    ├── jest.config.ts
    ├── .gitignore
    ├── .env.example
    ├── .env
    ├── README.md
    ├── CONTRIBUTING.md
    ├── __tests__/
    │   ├── jobs.test.ts
    │   └── indexer.test.ts
    └── src/
        ├── index.ts
        ├── indexer/
        │   ├── db.ts
        │   └── poller.ts
        └── routes/
            └── jobs.ts
```

### 🎯 Next Steps (Potential Ideas):
- Wire up other contract functions (fund, deliver, approve, dispute, resolve) to frontend
- Add support for multiple jobs in contract
- Add more comprehensive integration tests for backend
- Audit contract for security issues
- Close issue #1 once CI confirms green on the by-wallet upgrade

---

## June 22, 2026 (Session 2)
### ✅ By-Wallet Endpoint — Event-Indexed, Paginated, Role-Aware (Issue #1 Progress)

**Goal**: Replace the stub RPC-based `GET /api/jobs/by-wallet/:address` with a fast, SQLite-backed implementation that reads from the local event indexer.

#### What was built:

**`src/indexer/db.ts`** — Added `getJobsByWallet(address, page, limit)`:
- Queries `events` table for rows where `data_json LIKE %address%`
- Parses `data_json` as JSON and checks role fields strictly (`client`, `freelancer`, `arbiter`) — protects against false-positive LIKE matches on other string fields
- Groups events by `contract_id` using a `Map` — one distinct job per contract
- Each job summary captures: `contract_id`, `role`, `milestone_count`, `latest_event_type`, `latest_ledger`, `latest_timestamp`
- Applies `page`/`limit` pagination (1-based, safe clamping)
- Returns `{ jobs, total, page, limit }`
- Also exported `JobSummary` and `PaginatedJobs` TypeScript interfaces

**`src/routes/jobs.ts`** — Replaced old `by-wallet` handler:
- Removed live RPC call (`simulateTransaction` on a hardcoded single contract)
- Now calls `getJobsByWallet(address, page, limit)` directly from SQLite — sub-millisecond reads
- Validates `?page` and `?limit` query params with clear 400 errors
- Response shape: `{ success: true, jobs: [...], total, page, limit }`

**`__tests__/by-wallet.test.ts`** — 15 new tests:
- Returns empty when no events for address
- Role detection: client ✅ freelancer ✅ arbiter ✅
- Grouping: multiple events for same contract → single job entry
- Multi-contract: distinct jobs across C1/C2/C3
- False-positive guard: address in `token` field is NOT returned
- Pagination: page 1 of 2, page 2 of 2, last partial page, page beyond total
- HTTP integration (supertest): response shape, empty address, pagination params, job field types

#### Verification:
- `npx tsc --noEmit` → ✅ Zero errors
- `npm test` → ✅ 19/19 tests pass (3 suites: indexer, jobs, by-wallet)
- `npm run build` → ✅ Clean
- Commit: `9bd8508` — `feat: wire indexer into by-wallet endpoint with pagination and role detection`
- Pushed to GitHub, CI running

### 📁 Updated Project Structure:
```
Milesto/
├── escrow-contract/            # Soroban smart contract
│   ├── Cargo.toml
│   ├── Cargo.lock
│   ├── .gitignore
│   ├── README.md
│   ├── CONTRIBUTING.md
│   ├── context.md
│   └── contracts/
│       └── milestone-escrow/
│           ├── Cargo.toml
│           ├── src/
│           │   ├── lib.rs
│           │   └── test.rs
│           └── test_snapshots/

├── escrow-frontend/            # Next.js frontend
│   ├── package.json
│   ├── package-lock.json
│   ├── tsconfig.json
│   ├── next.config.ts
│   ├── tailwind.config.ts
│   ├── postcss.config.mjs
│   ├── .gitignore
│   ├── .env.local
│   ├── .env.local.example
│   ├── README.md
│   ├── CONTRIBUTING.md
│   ├── app/
│   │   ├── layout.tsx
│   │   ├── page.tsx
│   │   ├── globals.css
│   │   ├── lib/
│   │   │   └── contract.ts
│   │   ├── context/
│   │   │   └── WalletContext.tsx
│   │   ├── components/
│   │   │   ├── Navbar.tsx
│   │   │   ├── MilestoneCard.tsx
│   │   │   └── LoadingSkeleton.tsx
│   │   ├── create/
│   │   │   └── page.tsx
│   │   └── dashboard/
│   │       └── page.tsx
│   └── public/

└── escrow-backend/             # Express backend
    ├── package.json
    ├── package-lock.json
    ├── tsconfig.json
    ├── jest.config.ts
    ├── .gitignore
    ├── .env.example
    ├── .env
    ├── README.md
    ├── CONTRIBUTING.md
    ├── __tests__/
    │   ├── jobs.test.ts
    │   ├── indexer.test.ts
    │   └── by-wallet.test.ts     ← NEW
    └── src/
        ├── index.ts
        ├── indexer/
        │   ├── db.ts              ← getJobsByWallet() + interfaces added
        │   └── poller.ts
        └── routes/
            └── jobs.ts            ← by-wallet endpoint rebuilt
```

### 🎯 Next Steps (Updated):
- Confirm CI green on `9bd8508`, then close issue #1 with proper comment
- Wire remaining contract functions (fund, deliver, approve, dispute, resolve) to frontend
- Add support for multiple jobs in contract
- Audit contract for security issues
- Deploy and sync generated issues to GitHub using the `sync.js` script

---

## June 22, 2026 (Session 3)
### ✅ Collaborator Project Roadmap & Issue Generator (1,000 Issues)

**Goal**: Break the next major phases of Milesto down into 10 distinct phrases, each comprising 100 independent, well-labelled issues (1,000 issues total) to facilitate parallel development with collaborators, including a tool to preview and sync them.

#### What was built:

**`milesto-roadmap/generate.js`**:
- Programmatic Node.js generator.
- Splits development into 10 phrases across core domains: contract safety, advanced features, API security, indexer performance, wallet integrations, component libraries, admin/arbitrator portals, financial math, notification triggers, and monitoring/telemetry.
- Compiles exactly 100 independent issues per phrase, each referencing the target repository, validation check, implementation steps, testing guidelines, and difficulty labels.
- Outputs structured JSON issue templates into local directories under `milesto-roadmap/issues/`.

**`milesto-roadmap/sync.js`**:
- Interactive CLI import utility.
- Supports **Dry-Run Mode** (logs exact `gh` CLI commands to execute) and **Live Mode** (creates issues via local authenticated `gh` CLI credentials).
- Automatically parses and resolves the owner/repo mapping for the target packages (`escrow-contract`, `escrow-backend`, `escrow-frontend`) by checking git remote urls.

#### Verification:
- Ran `node generate.js` → successfully generated 10 folders, each containing exactly 100 issue JSON configurations (1,000 total).
- Tested `echo "1" | node sync.js` → successfully verified dry-run CLI command formatting and github remote mapping resolver.

### 📁 Updated Project Structure:
```
Milesto/
├── milesto-roadmap/            # NEW: Project Roadmap & Issues Manager
│   ├── generate.js             # Generates the 1000 issue templates
│   ├── sync.js                 # Interactive sync tool (dry-run & live)
│   └── issues/                 # Generated issues structured by phrase
│       ├── phrase_01_soroban_contract_safety_gas_optimization/
│       ├── phrase_02_advanced_escrow_features/
...     └── phrase_10_performance_monitoring_diagnostics/

├── escrow-contract/            # Soroban smart contract
│   ├── Cargo.toml
│   ├── Cargo.lock
│   ├── context.md
│   └── contracts/
│       └── milestone-escrow/

├── escrow-frontend/            # Next.js frontend
│   ├── package.json
│   └── app/

└── escrow-backend/             # Express backend
    ├── package.json
    └── src/
```


## October 15–27, 2026
### ✅ Stellar Wave campaign: audit-based issue batch
- Audited `main` of all three repos (backend ac5c192, contract 1884c5c, frontend ae62f6e) by running lint, type-check, tests and coverage, and filed **375 issues (125 per repo)**. Each one cites file:line, proposes a fix and lists acceptance criteria.
- Uploaded October 15 with `milesto-roadmap/sync_campaign_2026_10.js`. The drafts are in `milesto-roadmap/campaign_2026_10/`.
  - Backend #597–#722, contract #664–#788, frontend #569–#693.
  - Every issue carries a `p1`/`p2`/`p3` priority and the `Stellar Wave` label.

### 📈 Progress by October 27
| Repo | Campaign issues closed | PRs merged since Oct 15 | Contributors |
|---|---|---|---|
| escrow-backend | 111 / 125 (+11 fixed but not auto-closed) | 93 | 39 |
| escrow-contract | 107 / 125 | 110 | 38 |
| escrow-frontend | 101 / 125 | 68 | 37 |

### ⏭️ p1 issues still open
- **Contract:**
  - #674: the fee is never applied on release and there's no treasury address.
  - #672: a PartiallyReleased milestone can't be auto-released.
  - #690: the cancel and emergency refund functions don't transfer funds.
  - #684: the admin streaming override needs the parties' signatures.
  - #676: calculation functions emit spoofable events.
- **Frontend:**
  - #572: there's only one job per contract (needs a factory).
  - #573: the client becomes the escrow admin.
  - #570: success is shown before on-chain confirmation.
  - #584: CSP is report-only for now.
- **Backend:** #604–#606 and #675–#682 are fixed on `main`, but they still need a maintainer to close them. Their PRs merged into a feature branch, or reached main through a follow-up PR without the closing keyword.

### 🔎 Known gaps
- The frontend sends CSP reports to `/api/csp-report`, but the backend has no handler for it, so reports get a 404 in production (frontend #685).
- The frontend's batched deadline call needs a backend `GET /api/jobs/:id/deadlines` route. Until it exists, the frontend falls back to one request per milestone (frontend #671).
