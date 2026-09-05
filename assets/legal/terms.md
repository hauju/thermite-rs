# Terms of Service

**Effective Date:** [Last Updated: August 26, 2026]

## 1. Introduction / Acceptance of Terms
Thermite is provided by Hauke Jung ("we," "us," or "our"). By accessing or using the platform, pointing an SDK at our ingest endpoint, or maintaining an account, you agree to be bound by these Terms of Service ("Terms") and any policies referenced herein. If you do not agree, you may not use the service. These Terms govern your access to and use of Thermite, available at [https://thermite.oxidt.com](https://thermite.oxidt.com), and any related services we provide.

## 2. Description of Service
Thermite is an error tracking service that speaks Sentry's wire protocol. Unmodified Sentry SDKs report crashes into it, events are grouped into issues, and the same data is served to a hosted dashboard, a REST API and an MCP server that coding agents can read from and write their diagnoses back to. The service also includes per-project alert routing by email and webhook, cron monitoring for scheduled jobs, release health, documentation and related support resources. Thermite is primarily intended for business and organizational use within the European Union. If you use the Service as a consumer, statutory consumer protection rights under applicable EU or German law apply.

## 3. User Accounts
- You must be at least 18 years old and have the legal capacity to enter into a contract to create an account.
- Registration requires accurate, complete information and ongoing updates to keep your profile current.
- You are responsible for safeguarding login credentials and for all activity that occurs under your account.
- Authentication is powered by FerrisKey (self-hosted). You must comply with FerrisKey's usage policies in addition to these Terms.
- Notify us immediately at mail@haukejung.de if you suspect unauthorized access or any security breach.
- We may suspend or terminate accounts that violate these Terms, are inactive for extended periods, or pose security risks.

## 4. DSN Keys, API Keys and Access Tokens
- A DSN key authenticates event submission for one project. Treat it as a shared secret: anyone holding it can write events into that project and consume its quota. DSN keys embedded in client-side applications are visible to your users — that is inherent to the protocol, and rate limits and quotas exist for that reason.
- API keys and OAuth tokens grant read and write access to your issue data, including through the MCP interface. Keep them confidential and revoke them in the dashboard when they are no longer needed.
- You are responsible for all activity performed with keys issued to your account, including by agents and automations you connect.

## 5. Acceptable Use Policy
When using Thermite, you agree to:
- Comply with all applicable EU, German, and local laws, including GDPR.
- Only submit event data you have the right to transmit and store.
- Refrain from submitting unlawful, defamatory, discriminatory, or malicious content.
- Not misuse the service to distribute spam or malware, or to conduct penetration tests without prior written consent.
- Avoid interfering with or disrupting the platform, its infrastructure, or other users' access.
- Respect the rate limits, per-project event quotas and payload size limits documented in the dashboard and API.
- Not use the ingest endpoint as general-purpose log storage, an analytics pipeline, a metrics backend, or a data transport for content that is not an error report. It exists to receive crash reports; deliberately generating events to store unrelated data is a misuse of it.
- We reserve the right to review or remove content that violates these Terms or applicable laws, and to apply technical limits where usage materially exceeds ordinary use, contacting you beforehand where reasonably possible.

## 6. Intellectual Property Rights
- The platform, its code, design, and documentation remain the intellectual property of Hauke Jung or its licensors.
- Subject to compliance with these Terms, we grant you a non-exclusive, non-transferable license to use the service during the term of your account.
- You retain all rights to the event data you submit. You grant us a limited license to receive, store, group and process that data solely to provide the service to you.
- All trademarks, logos, and marks used on the service belong to Hauke Jung or third parties. You may not use them without prior written consent. Sentry is a trademark of Functional Software, Inc.; Thermite implements a compatible wire protocol and is neither affiliated with nor endorsed by them.

## 7. Your Data and Your Responsibilities
- We process personal data in accordance with our Privacy Policy, available at [Privacy Policy](https://thermite.oxidt.com/legal/privacy). By using the service, you acknowledge that processing.
- **You decide what your applications transmit.** Stack traces, request headers and bodies, breadcrumbs and user context reach us because your SDK was configured to send them. You remain the controller of that data and warrant that you have a lawful basis to send it.
- You must not deliberately transmit special categories of personal data (Article 9 GDPR) in error reports.
- We scrub values under a list of credential-like keys before anything is written, and you can extend that list. Scrubbing is key-based and best-effort; use your SDK's own filtering hooks for anything that must never leave your systems. Section 4 of the Privacy Policy explains the limits.
- Core subprocessors are FerrisKey (self-hosted authentication) and our outbound email provider. Each operates with EU data residency or appropriate safeguards. Contact us if you require a data processing agreement.

## 8. Agents, MCP and Automated Access
- Thermite never sends your data to a language model on its own initiative. It exposes an MCP endpoint so that a client you connect can read issues and write analyses back.
- Connecting an agent transmits the issue data it reads — stack traces, breadcrumbs, tags and attached context — to that agent and, where the agent is backed by a hosted model, to that model's provider under their terms. That transfer is initiated by you. You are responsible for deciding whether the content of your error reports may be sent there, and for any onward processing by that provider.
- Analyses written back by an agent are stored on the issue and visible to everyone with access to the project. We do not review them for accuracy; they are a diagnostic aid, not advice you should act on unverified.

## 9. Availability, Quotas and Data Retention
- Events are retained for a limited period and up to a per-project cap, whichever applies first, as described in the Privacy Policy and the documentation. Once an event is evicted it cannot be recovered. Export anything you need to keep.
- When a project exceeds its quota, further events are rejected with a rate-limit response until the quota resets. Rejected events are counted but not stored.
- We may change default limits with reasonable notice. We do not guarantee any specific availability level unless separately agreed in writing.

## 10. Payment Terms
- The Service is currently offered without charge. There are no paid plans, and no payment details are collected.
- If paid plans are introduced, we will give reasonable advance notice and no charge will be applied to an existing account without your explicit agreement.
- If you are a consumer in the EU, your statutory right of withdrawal under §355 BGB (German Civil Code) remains unaffected should paid plans be introduced.

## 11. Disclaimers and Warranties
- The service is provided "as is" and "as available." We do not warrant uninterrupted, error-free operation.
- Except as required by law, we disclaim all implied warranties, including merchantability, fitness for a particular purpose, and non-infringement.
- We do not warrant that every event your SDK sends will be accepted, stored, or retained, that every alert will be delivered, or that grouping will always assign an event to the issue you would have chosen. **Thermite must not be relied on as the sole mechanism for detecting outages or safety-critical failures.**
- We do not guarantee that the service will meet your expectations or produce specific business outcomes.

## 12. Limitation of Liability
- To the maximum extent permitted by law, Hauke Jung shall not be liable for any indirect, incidental, special, consequential, or punitive damages, or for lost profits or revenues, including damages arising from an alert that was not delivered or an event that was not retained.
- Where the Service is provided without charge, our aggregate liability for any claim arising from it is limited to the maximum extent permitted by law. Where fees have been paid, our aggregate liability will not exceed the fees you paid to us in the twelve (12) months preceding the event giving rise to the claim.
- These limitations apply even if we have been advised of the possibility of such damages and regardless of the theory of liability.
- Nothing in these Terms limits liability for intent (Vorsatz), gross negligence (grobe Fahrlässigkeit), injury to life, body, or health, or under mandatory consumer protection laws.

## 13. Termination
- You may terminate your account at any time within the dashboard or by contacting mail@haukejung.de.
- We may suspend or terminate your access if you violate these Terms or create security or legal risks.
- Upon termination, your license to use the platform ends immediately and your projects and their events may be deleted. We may retain backup copies for a limited period as required by law or to fulfill contractual obligations.
- You are responsible for exporting your data before termination becomes effective.
- Termination does not relieve you of any obligations incurred prior to the termination date.

## 14. Governing Law and Dispute Resolution
- The contracts concluded between the Provider and the customer are subject to the law of the Federal Republic of Germany to the exclusion of the UN Convention on Contracts for the International Sale of Goods.
- Disputes should first be resolved amicably. If unresolved within thirty (30) days, they will be submitted to binding arbitration under the rules of the German Arbitration Institute (DIS), seated in Berlin, unless otherwise required by mandatory consumer law.
- The United Nations Convention on Contracts for the International Sale of Goods does not apply.
- Nothing in this section prevents either party from seeking injunctive relief in competent courts within Germany.

## 15. Changes to Terms
- We may update these Terms to reflect changes in law, features, or our business practices.
- Material changes will be announced via the dashboard, email, or other reasonable notice at least fourteen (14) days before they take effect, unless immediate changes are required by law.
- Continued use of the service after the effective date constitutes acceptance of the updated Terms.

## 16. Severability and Entire Agreement
If any provision of these Terms is found to be invalid or unenforceable, the remaining provisions will remain in full force and effect.
These Terms, together with the Privacy Policy and any referenced documents, constitute the entire agreement between you and Hauke Jung regarding Thermite.

## 17. Contact Information
For questions or concerns about these Terms, contact us at:

Hauke Jung
Hauptstr. 41
79199 Kirchzarten, Germany
**Email:** [mail@haukejung.de](mailto:mail@haukejung.de)
**Website:** [https://thermite.oxidt.com](https://thermite.oxidt.com)

---

© [2026] Hauke Jung. All rights reserved.
