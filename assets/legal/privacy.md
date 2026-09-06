# Privacy Policy

**Effective Date:** [Last Updated: September 6, 2026]

Welcome to Thermite (the "Service"). This Privacy Policy explains how **Hauke Jung** ("we," "us," or "our") collects, uses, and protects personal information in connection with our website **[https://thermite.rs](https://thermite.rs)** and related services.

Thermite is an error tracker. That shapes this policy: most of the data it holds is not information you typed into a form, but crash reports your own applications transmit to it automatically. Sections 3 and 4 describe exactly what those reports contain, what is removed before anything is stored, and who is legally responsible for it.

We are committed to protecting your privacy and handling data transparently and responsibly in accordance with the **General Data Protection Regulation (GDPR)** and other applicable laws in Germany and the European Union.

---

## 1. Introduction

This Privacy Policy describes how **Hauke Jung**, operating from **Hauptstr. 41, 79199 Kirchzarten, Germany**, processes personal information in connection with **Thermite**, an error tracking service that receives crash reports from applications over Sentry's wire protocol, groups them into issues, and exposes them to a dashboard, a REST API and an MCP server for coding agents.

We are committed to protecting your data and maintaining transparency about our data processing practices.

---

## 2. Our Two Roles: Controller and Processor

Thermite holds two very different kinds of data, and our legal role differs for each.

**As a controller**, we process the data of the people who hold accounts with us: your registration details, your login sessions, your project configuration and the messages you send us. Sections 3(a), 3(b) and 8 describe that data.

**As a processor** (Article 28 GDPR), we store and serve back the error reports your applications send us. **You decide what your application transmits** — which SDK you install, which integrations you enable, whether you attach user context, and whether you send request bodies or local variables. We do not choose that content and we do not use it for our own purposes. You remain the controller of it towards the people whose data appears in your crash reports, and you are responsible for having a lawful basis to send it. Section 4 describes what such reports typically contain.

If you need a data processing agreement (Auftragsverarbeitungsvertrag) covering this, contact us using the details in Section 16.

---

## 3. Information We Collect

### a. Information You Provide Directly
We collect information you voluntarily provide to us, including:
- Account registration details (name, email address, authentication data)
- Project configuration you create (project names, DSN keys, component key labels, alert recipient addresses and webhook URLs, retention and quota settings)
- Feedback and communication (messages, support requests)

### b. Information Collected Automatically About Your Use of the Dashboard
When you sign in and use the dashboard, we automatically process:
- The IP address of your browser, for rate limiting and abuse prevention
- Device and browser information sent in ordinary HTTP request headers
- Server log data, such as access times and requested paths
- Your session cookie and, in your browser's local storage, your light/dark theme preference

We run **no web analytics product**, no advertising or marketing trackers, and no third-party tracking scripts on this site.

### c. Error Report Data Sent by Your Applications
This is the bulk of what Thermite stores. Your applications send it through a Sentry-compatible SDK using a DSN key you generate. Section 4 sets out what it contains.

### d. Information from Third Parties
We may receive limited information from providers that help us operate the Service:
- **FerrisKey (self-hosted)** for user authentication and secure login
- Our **outbound email (SMTP) provider**, used to deliver alert notifications you have configured

---

## 4. Error Report Data in Detail

### a. What an error report can contain

Sentry-compatible SDKs assemble a crash report automatically. Depending on the SDK, the platform and your configuration, an event we receive and store may include:

- **The exception itself** — exception type, message, and the full stack trace: source file paths, module and function names, line numbers, and — if your SDK is configured to send them — surrounding source lines and the values of local variables in each frame.
- **Breadcrumbs** — the trail of log lines, navigation events, database queries and outbound HTTP calls your application recorded in the seconds before the crash.
- **Request context** — the URL and query string, HTTP method, request headers, cookies and, if your SDK sends it, the request body of the request that failed.
- **User context** — whatever your SDK attaches as the affected user: an id, a username, an email address and/or an IP address.
- **Runtime context** — operating system, runtime and SDK name and version, device information, `server_name`.
- **Tags and metadata** — environment, release identifier, transaction name, the component label of the key it was reported through, and any custom tags or `extra` context your code attaches.
- **Cron check-ins** — for scheduled jobs monitored through Thermite: the job's slug, schedule, timezone, status and duration.
- **Session counters** — for release health, aggregate counts of started and crashed sessions per release. These are **counters, not records**: no per-session row and no session identifier is stored.

We do not add anything to a report. Everything above is data your application chose to transmit.

### b. What is removed before storage

Crash reports routinely contain live credentials, because SDKs attach headers, cookies and form bodies by default. Thermite therefore **scrubs the payload at ingest, before it is written to the database**. Any value whose key matches one of the following is replaced with `[Filtered]`:

`password`, `passwd`, `secret`, `token`, `api_key`, `authorization`, `cookie`, `session`, `credentials`, `private_key`

Matching is case-insensitive, ignores hyphens and underscores, and matches substrings, so `X-Api-Key`, `apikey`, `Set-Cookie` and `access_token` are all caught. It runs recursively over the whole payload, and a matching key's value is discarded whole rather than walked into — an entire `cookies` object disappears, because every cookie in it is a credential. The self-hosted deployment option lets you extend this list with your own keys.

Two honest caveats: scrubbing is **key-based**, so a secret placed in a value under an innocuous key (say, a full URL with a token in its query string) is not detected; and scrubbing is **not retroactive** — it protects data that has not been written yet, not data already stored. Configure your SDK's own `before_send` hook for anything you must never transmit in the first place.

### c. IP addresses

Two distinct IP addresses are involved, and they are treated differently:

- **The IP of the client that submits a report** is seen by our ingest endpoint and used for rate limiting and abuse prevention. It is not written into the stored event.
- **`user.ip_address` inside the report**, if your SDK attaches it, is stored as part of the event payload and is deleted with that event when retention expires it (Section 10). It is deliberately **never** used as a user identity: it is not counted towards "users affected" and it is never written into the long-lived tag rollup that outlives individual events. That is a design decision precisely so that IP addresses do not accumulate in a table retention cannot reach. Most SDKs let you disable IP collection (`send_default_pii = false`, or equivalent); we recommend it if you do not need it.

### d. What outlives an event

When retention deletes an event, an aggregate record of the issue it belonged to remains: its title, exception type and message, first and last seen timestamps, how many times it occurred, how many distinct users it affected as a **number only**, and any diagnosis a coding agent wrote onto it. These aggregates hold no event payload — but note that an issue title is taken from the exception message, so an application that puts personal data into exception messages will see it persist there. Keep personal data out of exception messages.

---

## 5. How We Use This Information

We use account and usage data to:

- **Provide and maintain the Service** (authentication, ingest, storage, the dashboard and API)
- **Deliver the alerts you configure**, by email and webhook, to the recipients you specify
- **Enforce quotas, rate limits and retention policies**
- **Improve and secure** the Service
- **Communicate** with you about support, security and changes to the Service
- **Comply with legal obligations** under EU and German law

We use error report data **only to provide the Service to you**: to group, store, display, alert on and serve back your own events. We do not analyse it for our own purposes, do not use it to train any model, and do not use it to profile the individuals appearing in it.

Legal bases: performance of a contract (Art. 6(1)(b) GDPR) for operating your account; our legitimate interest (Art. 6(1)(f) GDPR) in keeping the Service secure and available for log data and rate limiting; and, for error report data, processing on your documented instructions as a processor (Art. 28 GDPR).

---

## 6. How We Share This Information

We do **not sell** personal information. We share it only as follows:

- **Service providers:** FerrisKey (self-hosted authentication) and our outbound email provider, which delivers the alert emails you configure. They process data on our behalf under data protection agreements.
- **Alert recipients you choose:** when you configure an alert email address or webhook URL, issue details — title, exception type and message, project, release, and a link to the issue — are transmitted to that address or endpoint. You control where that goes; a webhook you point at a third party sends your data to that third party.
- **Coding agents you connect:** see Section 7.
- **Legal requirements:** if required by law or a competent authority.
- **Business transfers:** in case of a merger, acquisition or asset sale.
- **With your consent:** when you explicitly authorize us to do so.

---

## 7. Coding Agents and the MCP Interface

Thermite exposes your issues over an MCP endpoint so that a coding agent can read a stack trace and write its diagnosis back. **Thermite itself never calls a language model.** No data leaves the Service through this interface unless you connect a client to it.

If you do connect one, understand what that means: the agent receives the issue's full detail — stack traces, breadcrumbs, tags and whatever context your SDK attached — and, if that agent is backed by a hosted model, it transmits that content to its own provider under that provider's terms. That transfer is initiated by you, not by us, and you are responsible for deciding whether the data in your crash reports may be sent there. Access requires an API key or an OAuth authorization you issue, and you can revoke either at any time in the dashboard.

Any analysis an agent writes back is stored on the issue and is visible to everyone with access to that project.

---

## 8. Cookies and Tracking Technologies

We use a single **strictly necessary** cookie: a signed session cookie that keeps you logged in, expiring after 7 days of inactivity. Requests that change state are additionally protected by an origin check (CSRF protection).

Your light/dark theme preference is stored in your browser's **local storage**, not in a cookie, and is never transmitted to us.

We set **no analytics, advertising or tracking cookies**, and embed no third-party tracking scripts. There is nothing here to consent to beyond what is technically required to log you in, which is why the site shows no cookie banner. See our [Cookie Policy](https://thermite.rs/legal/cookies) for the full detail.

---

## 9. Data Security

We take appropriate organizational and technical measures to protect data, including:
- HTTPS encryption for all data transmissions
- Server-side scrubbing of credentials from crash reports before storage (Section 4b)
- Project data isolation, so each project's events are reachable only through keys issued for it
- API keys and OAuth tokens stored only as hashes, never in plaintext
- Rate limiting and quotas on the ingest path
- Regular security updates and access controls
- Hosting data on secure servers located in Germany

However, no system is 100% secure. While we strive to protect your information, we cannot guarantee absolute security.

---

## 10. Data Retention

**Error events** are deleted by whichever of two rules applies first: an **age limit** (90 days by default) and a **per-project cap** on the number of stored events (100,000 by default). The same sweep also ages out the aggregate rollups that back the charts and tag filters, so the user identifiers those rollups carry are genuinely erased on the same schedule.

**Issue-level aggregates survive** — see Section 4(d) for exactly what remains and what it does not contain.

**Account data** is retained for as long as your account exists, and deleted or anonymized when you close it, except where a statutory retention period requires otherwise.

**Server logs and rate-limit counters** are short-lived and rotate automatically. **Login sessions** expire after 7 days of inactivity and expired sessions are pruned hourly.

You may request deletion of your data at any time (see Section 11, Your Rights and Choices).

---

## 11. Your Rights and Choices

As a user in the European Union, you have the following rights under the **GDPR**:

- **Access:** Request a copy of the personal data we hold about you.
- **Correction:** Request correction of inaccurate or incomplete information.
- **Deletion ("Right to be Forgotten"):** Request deletion of your personal data.
- **Restriction of Processing:** Ask us to limit the use of your data.
- **Data Portability:** Receive your data in a structured, machine-readable format.
- **Objection:** Object to processing based on legitimate interests.
- **Withdraw Consent:** Revoke consent at any time, without affecting prior lawful processing.

### If you appear in someone else's error report
If your personal data reached Thermite because a customer's application sent it in a crash report, that customer is the controller of it and is the right party to address your request to. We act on their instructions. If you contact us instead, we will forward your request to them where we can identify the relevant customer, and assist them in responding.

### Do Not Track
Our Service currently does not respond to "Do Not Track" browser signals. It also runs no tracking for such a signal to disable.

### CCPA (California)
If you are a California resident, you may have similar rights under the **California Consumer Privacy Act (CCPA)**. Requests can be made using the contact information below.

---

## 12. International Data Transfers

Your data is stored and processed **within the European Union**, primarily in Germany.
If data must be transferred outside the EU (e.g., through a sub-processor), we ensure appropriate safeguards such as **Standard Contractual Clauses (SCCs)** are in place.

Note that alert webhooks and MCP clients you configure yourself may send data outside the EU. Those transfers are yours to assess — see Sections 6 and 7.

---

## 13. Children's Privacy

Our Service is **not intended for children under 16 years of age**.
We do not knowingly collect personal information from minors.
If you believe a child has provided us with personal data, please contact us at [mail@haukejung.de](mailto:mail@haukejung.de) so we can delete it.

---

## 14. Third-Party Links

Our website and documentation may contain links to third-party sites.
We are not responsible for the content or privacy practices of these external websites.
We encourage you to review their privacy policies before sharing any personal data.

---

## 15. Changes to This Privacy Policy

We may update this Privacy Policy from time to time.
Any material changes will be notified via email or a prominent notice on our website.
Please review this policy periodically to stay informed about how we protect your data.

---

## 16. Contact Information

If you have any questions, concerns, or privacy requests, please contact:

**Data Controller:**
Hauke Jung
Hauptstr. 41
79199 Kirchzarten, Germany
**Email:** [mail@haukejung.de](mailto:mail@haukejung.de)

If you believe your data has been handled improperly, you also have the right to lodge a complaint with your local **Data Protection Authority (DPA)**, such as the **Landesbeauftragte für den Datenschutz und die Informationsfreiheit Baden-Württemberg (LfDI)**.

---

© [2026] Hauke Jung. All rights reserved.
