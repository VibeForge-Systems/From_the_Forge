# Is it yours to publish? (Phase 0)

**This is general information, not legal advice.** It tells you what to check and who decides; your employment agreement and its governing law decide, and an employment lawyer or your employer's open-source office can tell you how they apply to you.

## Contents

- Why this gate comes first
- What to check
- What the statutes do — and do not — say
- Florida
- How large companies run this
- Getting clearance

## Why this gate comes first

Every other phase can be redone. A release you had no right to make cannot be taken back: forks and archives keep it (`github-exposure.md`), and the dispute is about ownership, not about a leaked value you can rotate. Anyone with a day job writing software in the same field as their side project is in exactly the zone where invention-assignment clauses bite.

## What to check

- **Your signed agreements.** The invention-assignment or "proprietary information and inventions" agreement (often called a PIIA), its *prior inventions* schedule (anything you listed there is carved out), and any later amendments.
- **The governing-law clause.** It decides which state's statute, if any, applies — which may not be the state you live in.
- **The moonlighting / outside-activity policy.** Many require disclosure or approval of side projects, separately from IP ownership.
- **Resources.** Was any of it written on employer hardware, on paid time, inside an employer GitHub organization, or with an employer-provisioned AI coding seat? Each is a fact an employer can point to.
- **Relatedness.** Does it relate to the employer's business, or to work the employer is demonstrably planning? Does it draw on internal designs, customer engagements, or anything confidential?
- **Third parties.** Code written for a client belongs to whoever the contract says; check the client contract, not just your employment agreement.
- **Your own commit history.** The sweep's identity findings (`identity-and-leaks.md`) are evidence too: a commit authored with a work address is a fact about where and how it was written.

## What the statutes do — and do not — say

Several US states limit how far an invention-assignment clause can reach. The shared pattern, from California Labor Code §2870 (1979; the text has not changed in substance since 1986): a clause cannot reach an invention developed **entirely on your own time** and **without the employer's equipment, supplies, facilities or trade-secret information** — **unless** the invention (1) relates, when conceived or reduced to practice, to the employer's business or its actual or demonstrably anticipated research and development, or (2) results from work you did for the employer. A clause reaching further is unenforceable to that extent.

The exceptions are the point. An enterprise architect publishing architecture, DevOps or AI tooling is often squarely inside "relates to the employer's business", and the carve-out is lost there no matter whose laptop or weekend it was.

Related provisions in California: §2871 lets an employer still require confidential disclosure of all inventions so it can judge which are its own; §2872 requires written notice of the carve-out in agreements made after 1980, and puts the **burden of proof on the employee**.

| State | Statute | Notes |
|---|---|---|
| California | Lab. Code §§2870–2872 | Written notice required; burden on employee |
| Delaware | 19 Del. C. §805 | |
| Illinois | 765 ILCS 1060/2 | Written notice required |
| Kansas | K.S.A. 44-130 | Written notice required |
| Minnesota | Minn. Stat. §181.78 | Written notice required |
| New Jersey | N.J.S.A. 34:1B-265 (2017) | Protects against use of any employer *information*, not only trade secrets |
| New York | Labor Law §203-f (2023) | Effective on signing, 15 September 2023; no notice or burden-of-proof provisions |
| North Carolina | G.S. 66-57.1 | Burden on employee |
| Utah | Code §34-39-1 et seq. | Broad definition of "employment invention", reaching the employer's industry; a broader agreement can bind if it is not a condition of employment and has separate consideration |
| Washington | RCW 49.44.140 | Written notice required |
| Nevada | NRS 600.500 | The **opposite** direction: absent a written agreement, the employer owns inventions within the scope of employment that relate directly to the work |

A secondary source also lists North Dakota (N.D.C.C. §34-02-11, as read by the Eighth Circuit in 1983); not verified against the code. No new state statute was found for 2024–2026.

## Florida

Florida has **no** invention-assignment statute of this kind. Ownership turns on the contract and on common law: without an express assignment term, Florida courts apply the hired-to-invent doctrine — the employer generally has to show the employee was hired to make that specific invention (*State Board of Education v. Bourne*, 1942; *City of Cocoa v. Leffler*, Fla. 5th DCA 2002). Florida's restrictive-covenant statute (§542.335, extended by the 2025 CHOICE Act) governs non-competes and confidentiality, not IP assignment — but it often sits in the same agreement.

Two consequences: a broadly drafted assignment clause is not cut back by a Florida statute, and a **governing-law clause naming another state** can bring that state's statute (for or against you) into play.

## How large companies run this

Outbound open-source programs — Google's release process, Microsoft's, the TODO Group's "Outbound Open Source" guide — converge on the same steps, and many apply them to side projects too:

- a registered approval (business and legal review) before the repository is created;
- confirmation that all of the code can be released, including third-party code, which is isolated and attributed;
- removal of internal references, codenames, internal telemetry and email aliases;
- trademark and naming review, and in some programs an export-control (ECCN) check for cryptography;
- a disclaimer that the project is not an official product when it is not;
- a CLA or DCO for outside contributions;
- a plan for maintenance, and for end of life.

The TODO Group's guide also covers keeping work and personal identities apart in git — e.g. `includeIf "gitdir:~/personal/"` in `~/.gitconfig` to switch `user.email` by directory, which prevents the work-address commit before it happens.

## Getting clearance

Ask in writing, name the repository and what it does, and keep the answer. Where an open-source program office exists, use it; otherwise legal or your manager in writing. A clearance email is the artifact phase 0's gate checks for. Until you have it, stop.
