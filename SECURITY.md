# Security Policy

The milestone escrow contract custodies user funds. Please report
vulnerabilities privately so a fix can be prepared before details are public.

## Supported versions

Only the builds currently deployed are supported. Each one is identified by
the SHA-256 hash of its installed WASM, recorded under
[`deployments/`](deployments/) and in [`CHANGELOG.md`](CHANGELOG.md).

| Network | Contract ID | WASM SHA-256 |
|---|---|---|
| testnet | `CARZQMYKDGA4TTIE2XBDJ42EX27VYLLJGGG4WGUIXXWIVHPDTX3UCLJA` | `67d0eb543aaa9ca99c1b23ba18a3f4713c899eba574bbef96540f2540b8b2f0e` |

The current `main` branch is also in scope. Older builds that are no longer
deployed are not supported.

## Reporting a vulnerability

**Do not open a public issue, pull request or discussion for a security
problem.**

Report it through GitHub private vulnerability reporting: open the
repository's **Security** tab and choose **Report a vulnerability**
(<https://github.com/Goldii-locks/escrow-contract/security/advisories/new>).

Please include:

- the affected contract, function and commit or WASM hash;
- a description of the issue and its impact (for example, funds that can be
  taken, frozen or misdirected);
- steps or a test case that reproduce it;
- any suggested fix, if you have one.

## Response times

| Stage | Target |
|---|---|
| Acknowledge the report | within 3 business days |
| Initial assessment and severity | within 7 days |
| Status updates | at least weekly until resolved |
| Fix or mitigation for a critical issue | as fast as possible, normally within 30 days |

Once a fix is deployed, we publish a GitHub security advisory and credit the
reporter unless they ask to stay anonymous.

## Scope

In scope:

- the contracts under [`contracts/`](contracts/);
- deployed instances on **testnet** as well as mainnet;
- deployment and release scripts and workflows in this repository.

Out of scope:

- issues in third-party dependencies that are already publicly known (report
  them upstream);
- findings that need a compromised admin key, unless the contract lets that
  key exceed the powers documented in [`THREAT_MODEL.md`](THREAT_MODEL.md);
- denial of service through network spam or fee pressure.

## Safe harbour

We will not pursue or support legal action against anyone who, in good faith:

- follows this policy and reports privately;
- avoids privacy violations, data destruction and disruption of service;
- tests only against testnet, local networks, or accounts and funds they own;
- does not take, freeze or move funds belonging to others, and stops as soon
  as a vulnerability is confirmed;
- gives us reasonable time to fix the issue before disclosing it publicly.

If you are unsure whether an action is covered, ask in the private report
first.
