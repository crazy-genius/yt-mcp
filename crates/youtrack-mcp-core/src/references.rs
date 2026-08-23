pub(crate) const QUERY_SYNTAX: &str = r#"# YouTrack Search Query Language — Reference

> Source: JetBrains YouTrack Cloud 2026.2 official docs — "Search Query Reference"
> (`https://www.jetbrains.com/help/youtrack/cloud/search-and-command-attributes.html`, updated 22 July 2026).
> The YouTrack Server reference is equivalent for all constructs listed here.
> Intended use: MCP resource / system context for LLM-generated YouTrack queries.

## 0. Scope note for the model

- Everything below is the **search** language. Commands (issue updates) use a similar
  attribute–value shape but **do not** apply the rules for colons, braces, and `#`.
- Grammar is **case-insensitive** (attributes, values, `AND`/`OR`, keywords).
- Some constructs are unavailable in Simple Search; assume **Advanced Search**.
- Default field names, values and aliases can be customized per instance. Attribute
  names such as `State`, `Priority`, `Type`, `Subsystem`, `Assignee` and link type names
  are **defaults, not guarantees** — verify against the target instance before relying on them.

## 1. Grammar (BNF)

```
<SearchRequest>       ::= <OrExpression>
<OrExpression>        ::= <AndExpression> ('or' <AndExpression>)*
<AndExpression>       ::= <AndOperand> ('and' <AndOperand>)*
<AndOperand>          ::= '(' <OrExpression>? ')' | Term
<Term>                ::= <TermItem>*
<TermItem>            ::= <QuotedText> | <NegativeText> | <PositiveSingleValue>
                        | <NegativeSingleValue> | <Sort> | <Has> | <CategorizedFilter> | <Text>
<CategorizedFilter>   ::= <Attribute> ':' <AttributeFilter> (',' <AttributeFilter>)*
<Attribute>           ::= <name of issue field>
<AttributeFilter>     ::= ('-'? <Value>) | ('-'? <ValueRange>) | <LinkedIssuesQuery>
<LinkedIssuesQuery>   ::= ( <OrExpression> )
<ValueRange>          ::= <Value> '..' <Value>
<PositiveSingleValue> ::= '#' <SingleValue>
<NegativeSingleValue> ::= '-' <SingleValue>
<SingleValue>         ::= <Value>
<Sort>                ::= 'sort by:' <SortField> (',' <SortField>)*
<SortField>           ::= <SortAttribute> ('asc' | 'desc')?
<Has>                 ::= 'has:' <Attribute> (',' <Attribute>)*
<QuotedText>          ::= '"' <text without quotes> '"'
<NegativeText>        ::= '-' <QuotedText>
<Text>                ::= <text without parentheses>
<Value>               ::= <ComplexValue> | <SimpleValue>
<SimpleValue>         ::= <value without spaces>
<ComplexValue>        ::= '{' <value (can have spaces)> '}'
```

## 2. Core semantics (implicit operators)

| Situation | Implicit operator |
|---|---|
| Multiple **different** attributes | `AND` — `State: {In Progress} Priority: Critical` |
| Multiple values of **one** attribute (comma-separated) | `OR` — `State: {In Progress}, {To be discussed}` |
| Multiple words in a text search | `AND` — `State: Open context usage` matches both words |

`has: <attr>` and `<attr>: <value>` count as **separate** attributes, so they combine with `AND`:
`has: assignee Assignee: me` → assignee is set *and* it is you.

## 3. Symbols

| Symbol | Meaning | Example |
|---|---|---|
| `-` | Exclude a value / subset. With a single value, do **not** add `#`. | `#unresolved -minor` |
| `#` | The input is a single value (keyword, tag, project, state, issue ID, saved search, sprint). | `#my #unresolved in: MRK` |
| `,` | Value list for one attribute (OR). Combinable with ranges. | `created: Today, Yesterday` |
| `..` | Inclusive range between lower and upper bound. | `created: 2018-03-10 .. 2018-03-13` |
| `*` | Context-dependent wildcard: open range bound with `..`; zero-or-more chars at the **end** of an attribute value; zero-or-more chars in text search. | `created: * .. 2018-03-10`, `tag: refactoring*`, `attachments: *.png` |
| `?` | Any single character. **Text attributes only.** | `description: prioriti?e` |
| `{ }` | Wraps values containing spaces. | `tag: {to be tested}` |
| `" "` | Quoted text — exact phrase / forces text-search interpretation. | `summary: "Agile management"` |
| `( )` | Grouping; the parenthesized part is processed as one unit. | `(in: Kotlin #Critical or in: Ktor #Major) and for: me` |

## 4. Explicit operators

- `AND` / `OR`, case-insensitive (`and`, `And`, `aNd` all work).
- `AND` binds tighter than `OR` and is always processed as a group.
- **Whenever explicit operators are used, wrap every search argument in parentheses.**
  Otherwise repeated references to one attribute silently become `OR` and widen the result set.
- When parentheses are present, an explicit operator **must** join them to neighbouring terms:
  - INVALID: `in: Kotlin #Critical (in: Ktor and for: me)`
  - VALID:   `in: Kotlin #Critical or (in: Ktor and for: me)`

Examples:
```
in: Ktor and tag: {Next build} and tag: {to be tested}
in: Ktor #Critical or in: Kotlin #Major and for: me
in: Ktor for: me or tag: {to be tested}
(in: Kotlin #Critical or in: Ktor #Major) and for: me
```

## 5. Issue attributes

Syntax: `attribute: value`, `attribute: -value`, `attribute: v1, v2`.

| Attribute | Value type | Aliases | Notes / example |
|---|---|---|---|
| `attachment text` | text | `image text` | Text inside image attachments. `project: Design attachment text: design mockup` |
| `attachments` | text (filename) | — | `attachments: sketch*`, `attachments: *.png` |
| `Board <board name>` | sprint name | — | `Board YouTrack Scrum: {sprint 21}`; `{current sprint}` supported; boards without sprints → `has: <board name>` |
| `cc recipients` | user | — | Helpdesk tickets. `cc recipients: minnie.terry` |
| `customer groups` | customer group | — | `customer groups: {Acme Support}` |
| `code` | text | — | Matches code spans, fenced/indented blocks, stack traces in description & comments |
| `commented` | date \| period | — | Date of comments. For users use `commenter`. |
| `commenter` | user \| group | `commented by` | |
| `comments` | text | — | Text inside comments |
| `created` | date \| period | — | For users use `reporter`. |
| `description` | text | — | |
| `document type` | `Issue` \| `Ticket` | — | `#{Assigned to me} document type: Ticket` |
| `Gantt` | chart name | — | `Gantt: {Migrate to YouTrack}` |
| `has` | attribute name(s) | — | See §6 |
| `issue ID` | issue ID | — | `issue ID: JT-4232` or `#JT-4232`; see pitfalls §12 |
| `links` | issue ID | — | Issues linked to the given issue |
| `looks like` | issue ID | — | Summary/description word similarity |
| `mentioned in` | issue ID | — | Issues whose IDs are referenced in the target issue's description/comments |
| `mentions` | issue ID \| user | — | `mentions: valerie, -me`; `mentions: JT-4232` |
| `organization` | organization name | — | Also usable as single value: `#{Test cluster}` |
| `project` | project name \| project ID | `in` | Also `#ReSharper` |
| `reporter` | user \| group | `by`, `created by`, `reported by` | |
| `resolved date` | date \| period | — | `#MPS resolved date: {this month}` |
| `saved search` | saved search name | — | Also `#{resharper this week}` |
| `submitter` | user \| group | `submitted by` | Tickets created on behalf of another user |
| `summary` | text | — | |
| `tag` | tag name | `tagged as` | Also `#mytag` / `-mytag` |
| `updated` | date \| period | — | For users use `updater`. |
| `updater` | user \| group | `updated by` | |
| `vcs changes` | full SHA-1 commit hash | — | Short hashes are **not** recognized |
| `visible to` | user \| group | — | Also keyword `{issue readers}`; `visible to: -{issue readers}` finds restricted issues |
| `voter` | user \| group | `voted by` | |

## 6. `has:` (boolean presence)

`has: <attribute>` → attribute has a value. `has: -<attribute>` → attribute is empty.
Multiple attributes comma-separated: `in: TST for: me has: duplicates , attachments , -comments`

Supported with: `attachments`, `boards`, `Board <board name>`, `comments`, `description`,
`<custom field name>`, `Gantt`, `<link type name>`, `links`, `star`, `underestimation`
(spent time > estimation), `vcs changes`, `votes`, `work`.

Legacy `{has attachments}` still works but is not auto-completed.

## 7. Custom fields

Same syntax: `<field name>: <value>`, values often referenceable as `#value` / `-value`.
Wrap multi-word field names and values in `{}`.

Empty values:
- If the field has an explicit empty value: `Assignee: Unassigned` / `#Unassigned`.
- Otherwise: `<field name>: {No <field name>}` or `has: -<field name>`.

Default fields (customizable per instance):

| Field | Aliases | Extra keywords accepted |
|---|---|---|
| `Affected versions` | `affects`, `affecting`, `that affect` | `Released`, `Archived` |
| `Assignee` | `assigned to`, `for` | — (users/groups) |
| `Fix versions` | `fix for`, `fixed in`, `version` | `Released`, `Archived` |
| `Fixed in build` | `build`, `fix build`, `fix for`, `fixed in` | `Archived` |
| `Priority` | — | `Archived` |
| `State` | — | `Resolved`, `Unresolved`, `Archived` |
| `Subsystem` | `in` | `Archived` |
| `Type` | — | `Archived` |

Default resolved states: Fixed, Won't fix, Duplicate, Incomplete, Obsolete, Can't reproduce.
Default unresolved states: Submitted, Open, In Progress, Reopened, To be discussed.

## 8. Issue links and sub-queries

Two forms:
- `<link type>: <issue ID>` — issues linked to that specific issue via this link type.
- `<link type>: (<sub-query>)` — issues linked to **any** issue matching the sub-query.

Use the outward or inward name of the link type (custom link types work the same way).

| Term | Example |
|---|---|
| `links: <issue ID>` | `links: JT-5072` |
| `aggregate <aggregation link type>: <issue ID>` | `aggregate Subtask of: JT-5072` — indirect/transitive links |
| `Depends on` | `for: me #task Depends on: (#Unresolved)` |
| `Duplicates` | `Duplicates: (#Unresolved)` |
| `Is duplicated by` | `Is duplicated by: (State: {In Progress})` |
| `Is required for` | `for: me Is required for: JT-5072` |
| `Parent for` | `#Resolved Parent for: (#Unresolved)` |
| `Relates to` | `for: me Relates to: JT-5072` |
| `Subtask of` | `#Unresolved Subtask of: (Product: YouTrack)` |

Presence checks: `has: {Subtask of}` / `has: -{Subtask of}`.

## 9. Time tracking

| Attribute | Value | Example |
|---|---|---|
| `work` | text in work item | `work: test` |
| `work author` | user only | `in: TST work author: me` |
| `work type` | work item type; `{No type}` for untyped | `in: TST work type: Development` |
| `work date` | date \| period | `in: TST work date: {Last week}` |
| `work <attribute name>` | custom work item attribute | `in: TST work Expenses: Non-billable` |

## 10. Sorting

`sort by: <value> <asc|desc>` (alias: `order by`). Multiple sort fields comma-separated.

Sortable: `star`, `updated`, `updater`, `created`, `{resolved date}`, `project`, `reporter`,
`{issue id}`, `votes`, `summary`, `comments`, `<custom field>`, `{attachment size}`.

`relevance` **cannot** be specified as a sort attribute (text results may be sorted by it implicitly).

Example: `#my #fixed sort by: updated desc`

## 11. Keywords, dates and periods

### Keywords (used as `#keyword` or `-keyword`, no attribute)

| Keyword | Meaning |
|---|---|
| `me` | Current user. As a standalone `#me`: issues assigned to, reported by, or commented by the current user — plus any custom user-type field referencing them. |
| `my` | Alias for `me`. |
| `Resolved` | Resolved property (all state-type fields resolved). |
| `Unresolved` | Unresolved property (any state-type field not resolved). |
| `Released` | Version-field property. **Only** with a version field name/alias, never standalone: `fixed in: -Released`. |
| `Archived` | Version-field property. Only with a version field name/alias; with multi-value fields matches only when *all* versions are archived. |

### Absolute dates

Formats: `YYYY-MM-DD`, `YYYY-MM`, `MM-DD`; time `HH:MM:SS` or `HH:MM` (24h);
combined `YYYY-MM-DDTHH:MM:SS`.
Example: `created: 2010-01-01T12:00 .. 2010-01-01T15:00`

### Predefined relative parameters (evaluated in the current user's time zone)

`Now`, `Today`, `Tomorrow`, `Yesterday`,
`Sunday`, `Monday`, `Tuesday`, `Wednesday`, `Thursday`, `Friday`, `Saturday` (current week),
`{Last working day}` (per Time Tracking workday settings),
`{This week}`, `{Last week}`, `{Next week}`, `{Two weeks ago}`, `{Three weeks ago}`,
`{This month}`, `{Last month}`, `{Next month}`,
`Older` (1 Jan 1970 → last day of the month two months before now).

Weeks run 00:00 Monday → 23:59 Sunday.

### Custom relative periods

Syntax: `{minus <N><unit> ...}` for the past, `{plus <N><unit> ...}` for the future.
Units: `y`, `M` (month, capital M), `w`, `d`, `h`. Space-separated, whole numbers only:
`{minus 2y 3M 1w 2d 12h}`.

```
commented: {minus 7d} .. Today
updated: {minus 2h} .. *
created: * .. {minus 1y 6M} #Unresolved
Due Date: {plus 5d}
```

- Units below one hour (minutes, seconds) are **not supported**.
- Hour precision filters within that hour: at 15:35, `created: {minus 48h}` matches 15:00–16:00 two days ago, while `created: {minus 2d}` matches that whole day 00:00–23:59.
- `14d` and `2w` are equivalent.

## 12. Pitfalls for query generation

1. **Bare `JT-4232`** is parsed as both an issue-ID lookup *and* a text search (matches the
   substring anywhere in text attributes). Use `issue ID: JT-4232` / `#JT-4232` for the issue
   itself; `"JT-4232"` for text-only matching. Neither form searches issue **links** — use
   `links:` or a specific link type for that.
2. Repeating an attribute without explicit operators is `OR`, not `AND` — a frequent cause of
   over-broad results. For `enum[*]` fields and tags, join with explicit `AND`.
3. Multi-word values, field names, tags, sprints, saved searches and Gantt charts need `{}`.
4. `-` before a single value replaces `#`; never write `-#value`.
5. `?` works only on text attributes; `*` matches only at the **end** of an attribute value.
6. `Released` / `Archived` cannot stand alone as `#Released` — they require a version field.
7. `commented`, `created` and `updated` accept users too, but the user-oriented attributes
   (`commenter`, `reporter`, `updater`) are the explicit and safer choice.
8. Short VCS commit hashes are rejected — pass the full SHA-1.

## 13. Sample queries

```
project: IDEADEV author: andy.watkins , brianna.myers
project: IDEADEV created: 2022-01 .. 2022-06
project: IDEADEV created: * .. 2022-01
#my #unresolved in: MRK
#my created: today
#YouTrack for: me commented: {Last Week}
created: today for: me commenter: John
#MPS updated: {this month} #resolved
#bug reporter: yarko -minor -normal
priority: major visible to: {YouTrack Team}
in: TST for: me has: duplicates , attachments , -comments
in: Test fixed in: -Released
project: YouTrack Type: {Usability Problem} #Unresolved sort by: Priority asc
reported by: nadine OR commented by: nadine OR voted by: nadine
((project: Design, {Web UI}) or (project: Docs Assignee: raul)) and (#Unresolved)
(For: me) and ((state: {in progress}) or (state: {wait for reply} updated: * .. {last week}))
```"#;

pub(crate) const ISSUE_FIELDS: &str = r#"# YouTrack API — building the `fields` parameter for `Issue`

> Sources (JetBrains Developer Portal for YouTrack and Hub, verified 2026): entity references for
> `Issue`, `IssueCustomField`, `IssueComment`, `IssueAttachment`, `IssueLink`, `IssueLinkType`,
> `Tag`, `User`, `Project`; `api-fields-syntax.html`; `api-concept-pagination.html`;
> `resource-api-issues.html`; `operations-api-issues.html`;
> `api-how-to-update-custom-fields-values.html`.
> Purpose: this document tells the model **what to put into the optional `fields` argument**.
> Transport, auth and endpoints are handled by the client — do not reason about them here.

## 1. What `fields` does

`fields` is a projection. The server returns **only** what is listed there; with `fields`
omitted or empty, the response carries just the entity ID and `$type`. Nothing else comes back
"by default" — an absent attribute in the response almost always means it was absent from
`fields`, not that the data is missing.

`$type` is always included by the server and does not need to be requested, but it *can* be
requested explicitly inside nested selectors where the concrete subtype matters.

**Rule of thumb: request exactly the attributes the answer needs, nothing more.**
`fields` is the only lever controlling response size; over-requesting on a list endpoint is the
main cause of slow and oversized responses.

## 2. Syntax

```
fields = selector ("," selector)*
selector = name | name "(" fields ")"
```

- Comma-separated, **no spaces** (a space ends the parameter value unless encoded).
- Scalar attributes are written bare: `summary`, `created`, `votes`.
- Entity-typed and collection-typed attributes **must** have a parenthesised sub-selection:
  `project(name)`, `customFields(name,value(name))`. Writing `project` bare returns only the
  project's ID and `$type` — usually useless.
- Nesting is unbounded: `customFields(id,projectCustomField(field(name)),value(name))`.
- The same rules apply at every level; a nested collection still needs its own sub-selection.
- Case-sensitive, exactly as spelled in the tables below.

Valid: `id,idReadable,summary,project(shortName),reporter(login,fullName)`
Invalid intent: `id,summary,project,reporter` → project and reporter come back as bare IDs.

## 3. `Issue` — top-level selectors

**Scalars** (write bare):

| Selector | Type | Notes |
|---|---|---|
| `id` | String | Database ID (`2-42`). |
| `idReadable` | String | UI ID (`SP-38`). |
| `numberInProject` | Long | |
| `summary` | String | Nullable. |
| `description` | String | Raw markup. Nullable. |
| `wikifiedDescription` | String | Rendered HTML. **Expensive** — never on lists. |
| `created` | Long | ms, unix UTC. |
| `updated` | Long | ms, unix UTC. |
| `resolved` | Long | ms; `null` while unresolved. |
| `commentsCount` | Int | Cheap alternative to pulling `comments`. |
| `votes` | Int | Cheap alternative to pulling `voters`. |
| `isDraft` | Boolean | |

**Entity / collection** (require parentheses):

| Selector | Type | Sub-selector catalog |
|---|---|---|
| `project(...)` | Project | §4.2 |
| `reporter(...)`, `updater(...)`, `draftOwner(...)` | User | §4.1 |
| `customFields(...)` | Array of IssueCustomField | §5 |
| `tags(...)` | Array of Tag | §4.3 |
| `comments(...)`, `pinnedComments(...)` | Array of IssueComment | §4.4 |
| `attachments(...)` | Array of IssueAttachment | §4.5 |
| `links(...)`, `parent(...)`, `subtasks(...)` | IssueLink | §4.6 |
| `visibility(...)` | Visibility | §4.7 |
| `voters(...)`, `watchers(...)` | IssueVoters / IssueWatchers | §4.8 |
| `externalIssue(...)` | ExternalIssue | §4.8 |

## 4. Nested selector catalogs

### 4.1 `User`

`id`, `login`, `fullName`, `email`, `ringId`, `guest`, `online`, `banned`, `banBadge`,
`banReason`, `isAnonymized`, `avatarUrl`, `tags(...)`, `savedQueries(...)`, `profiles(...)`,
`userType(...)`.

Official samples also use `name` for a user's display name (`reporter(name)` → `"John Smith"`);
it is inherited rather than listed in the `User` table.

Practical picks: `reporter(login,fullName)` for display, `reporter(login)` for matching,
`reporter(id)` when only a reference is needed.

### 4.2 `Project`

`id`, `name`, `shortName`, `description`, `archived`, `template`, `iconUrl`, `startingNumber`,
`fromEmail`, `replyToEmail`, `leader(...)` (User), `createdBy(...)` (User),
`customFields(...)` (ProjectCustomField), `team(...)` (ProjectTeam), `issues(...)` (Array of Issue).

**Never request `project(issues(...))`** — it is every issue in the project.

Practical picks: `project(id,shortName,name)`.

### 4.3 `Tag`

`id`, `name`, `color(...)` (FieldStyle), `untagOnResolve`, `owner(...)` (User),
`readSharingSettings(...)`, `tagSharingSettings(...)`, `updateSharingSettings(...)`,
`issues(...)`, and the deprecated `visibleFor(...)` / `updateableBy(...)`.

**Never request `tags(issues(...))`** — same explosion as above.

Practical picks: `tags(id,name)`.

### 4.4 `IssueComment`

`id`, `text`, `textPreview` (rendered HTML, expensive), `created`, `updated`, `deleted`,
`pinned`, `author(...)` (User), `attachments(...)` (IssueAttachment), `reactions(...)`,
`visibility(...)`, `issue(...)` (back-reference — omit, it re-fetches the parent).

Practical picks: `comments(id,text,created,author(login,fullName))`.

### 4.5 `IssueAttachment`

`id`, `name`, `size`, `extension`, `mimeType`, `charset`, `metaData`, `created`, `updated`,
`draft`, `removed`, `url`, `thumbnailURL`, `base64Content`, `author(...)` (User),
`visibility(...)`, `issue(...)`, `comment(...)`.

**`base64Content` inlines the whole file** as a data URI — request it only when the bytes are
actually needed, never on a list. Prefer `url`.

Practical picks: `attachments(id,name,mimeType,size,url)`.

### 4.6 `IssueLink` and `IssueLinkType`

`IssueLink`: `id`, `direction` (`OUTWARD` | `INWARD` | `BOTH`), `linkType(...)`,
`issues(...)` (Array of Issue), `trimmedIssues(...)`.

`IssueLinkType`: `id`, `name`, `localizedName`, `sourceToTarget` (outward name),
`localizedSourceToTarget`, `targetToSource` (inward name), `localizedTargetToSource`,
`directed`, `aggregation`, `readOnly`.

The nested `issues(...)`/`trimmedIssues(...)` are full `Issue` entities — keep their sub-selection
to identifiers only. For issues with many links, use `trimmedIssues(...)` together with the
`$topLinks` / `$skipLinks` request parameters instead of `issues(...)`.

Practical picks:
```
links(direction,linkType(name,sourceToTarget,targetToSource,directed,aggregation),issues(id,idReadable,summary))
parent(issues(idReadable,summary))
subtasks(issues(idReadable,summary))
```

### 4.7 `Visibility`

`Visibility` is polymorphic — request `$type` to distinguish unrestricted from restricted
visibility. The concrete subtype's attribute names are **not covered by the sources used for
this document**; probe them against the target instance before relying on a specific spelling,
and fall back to `visibility($type)` when only "is it restricted" matters.

### 4.8 Opaque wrappers

`voters`, `watchers` (`IssueVoters` / `IssueWatchers`) and `externalIssue` are container
entities whose attribute lists are not covered here. Prefer the scalar `votes` and
`commentsCount` where they answer the question. If one of these is genuinely needed, request
`$type` plus the specific attribute confirmed against the instance.

## 5. `customFields` — the part that needs care

`IssueCustomField` has exactly four selectors:

| Selector | Meaning |
|---|---|
| `id` | ID of the field *in this issue* (not the field definition). |
| `name` | Field name as configured in the project. |
| `projectCustomField(...)` | Project-level settings; the definition name lives at `projectCustomField(field(name))`. |
| `value` / `value(...)` | The assigned value. |

### The `value` problem

`value` is polymorphic. Its shape depends on the field type:

- **Primitive** — String, Integer, Float, Date, Date and Time → `value` is a bare scalar,
  so write `value` **without** parentheses.
- **Entity** — `EnumBundleElement`, `StateBundleElement`, `VersionBundleElement`,
  `BuildBundleElement`, `OwnedBundleElement`, `User`, `UserGroup` → write `value(...)` **with**
  a sub-selection.
- **Array** of either, for multi-value fields.

Since a single request usually spans fields of several types, use a union sub-selection that
covers all shapes at once. This is the selector JetBrains uses in its own samples:

```
customFields(id,name,value(avatarUrl,buildLink,color(id),fullName,id,isResolved,localizedName,login,minutes,name,presentation,text))
```

Attributes not applicable to a given value type are simply absent from that entry. `name` covers
bundle elements and users; `login`/`fullName`/`avatarUrl` cover users; `isResolved` marks
resolved states; `minutes` covers period values; `text`/`presentation` cover text-ish values.

A lighter union that is enough for most reporting:
```
customFields(name,value(name,login,fullName,minutes,text))
```

### Which custom fields come back

- By default the response contains **all** custom fields of the issue.
- The separate `customFields` **request parameter** (not the `fields` selector) narrows this:
  pass it once per field name, and only those fields are returned. Combine both:
  `fields=id,summary,customFields(name,value(name))` + `customFields=priority&customFields=assignee`.
- Field names and types are per-project configuration. Never assume `Priority`, `State`,
  `Assignee` exist or have a given type on an unknown instance — read them first via
  `customFields(name,projectCustomField(field(name)),value(name))` and reuse what comes back.

## 6. Ready-made presets

Pick the smallest preset that answers the question; extend rather than starting from the widest.

| Task | `fields` |
|---|---|
| Bare reference / count | `id,idReadable` |
| List / search results | `id,idReadable,summary,created,updated,resolved,project(shortName),reporter(login,fullName)` |
| List with status | previous + `customFields(name,value(name,login,fullName))` |
| Triage view | `idReadable,summary,created,updated,commentsCount,votes,tags(name),customFields(name,value(name,login,fullName,isResolved))` |
| Single issue, full body | `id,idReadable,summary,description,created,updated,resolved,project(id,shortName,name),reporter(login,fullName),updater(login,fullName),tags(id,name),customFields(id,name,value(avatarUrl,buildLink,color(id),fullName,id,isResolved,localizedName,login,minutes,name,presentation,text))` |
| Comment thread | `id,idReadable,comments(id,text,created,updated,author(login,fullName),pinned,deleted)` |
| Attachments | `id,idReadable,attachments(id,name,mimeType,size,url,created,author(login))` |
| Link graph | `id,idReadable,links(direction,linkType(name,sourceToTarget,targetToSource,directed,aggregation),issues(id,idReadable,summary))` |
| Hierarchy only | `idReadable,summary,parent(issues(idReadable,summary)),subtasks(issues(idReadable,summary))` |
| Timeline / metrics | `id,idReadable,created,updated,resolved,commentsCount,votes,project(shortName)` |

## 7. Cost rules

Ordered from most to least important:

1. **Never** put `comments`, `attachments`, `links`, `wikifiedDescription` or `textPreview` into a
   list request. Fetch the list with identifiers, then fetch details per issue.
2. **Never** traverse back-references: `project(issues(...))`, `tags(issues(...))`,
   `comments(issue(...))`, `attachments(issue(...))`. Each pulls a whole collection or re-fetches
   the parent.
3. Keep nested `issues(...)` inside `links` limited to `id,idReadable,summary`. Use
   `trimmedIssues(...)` with `$topLinks`/`$skipLinks` when link counts are large.
4. `base64Content` inlines file bytes — only when the file content is the goal.
5. `description` is large; on lists prefer `summary` alone.
6. Collections cap at 42 entries without `$top`; for `GET /api/issues` the cap is the instance's
   **Max issues to export** setting. A short-looking response may be a truncated one — paginate
   rather than widening `fields`.

## 8. Checklist before emitting `fields`

- Every entity-typed selector has parentheses; every scalar has none.
- No selector present that the answer will not use.
- No back-reference or whole-collection traversal.
- If custom fields are involved: union `value(...)` selector, plus `name` so entries can be told
  apart.
- If the user asked about status/priority/assignee: those are custom fields, not top-level
  attributes — they live under `customFields`, never as `state`/`priority`/`assignee`.
- Timestamps returned (`created`, `updated`, `resolved`) are **milliseconds**."#;
pub(crate) const ARTICLE_FIELDS: &str = r#"# YouTrack API — building the `fields` parameter for `Article`

> Sources (JetBrains Developer Portal for YouTrack and Hub, verified 2026): entity references for
> `Article`, `ArticleComment`, `ArticleAttachment`, `Tag`, `User`, `Project`;
> `api-fields-syntax.html`; `api-concept-pagination.html`; `resource-api-articles.html`;
> `operations-api-articles.html`; `resource-api-articles-articleID-childArticles.html`.
> Purpose: this document tells the model **what to put into the optional `fields` argument**.
> Transport, auth and endpoints are handled by the client — do not reason about them here.
> `Article` is the knowledge-base entity and extends `BaseArticle`.

## 1. What `fields` does

`fields` is a projection. The server returns **only** what is listed there; with `fields`
omitted, the response carries just the entity ID and `$type`. A missing attribute in a response
almost always means it was missing from `fields`.

`$type` is always returned and need not be requested, though it can be requested inside nested
selectors when the concrete subtype matters.

**Request exactly what the answer needs.** For articles this matters more than for issues,
because `content` is a full document body and `childArticles` is a tree.

## 2. Syntax

```
fields = selector ("," selector)*
selector = name | name "(" fields ")"
```

- Comma-separated, no spaces.
- Scalars bare: `summary`, `content`, `created`.
- Entity- and collection-typed attributes **must** carry a parenthesised sub-selection:
  `project(shortName)`, `reporter(name)`. A bare `project` yields only its ID and `$type`.
- Nesting is unbounded and follows the same rules at every level.
- Case-sensitive.

## 3. `Article` — top-level selectors

**Scalars** (write bare):

| Selector | Type | Notes |
|---|---|---|
| `id` | String | Database ID (`226-0`). |
| `idReadable` | String | UI ID (`NP-A-1`); the `-A-` infix marks an article. |
| `summary` | String | **The article title.** Nullable. |
| `content` | String | Article body, raw Markdown. Nullable. **Large.** |
| `created` | Long | ms, unix UTC. |
| `updated` | Long | ms, unix UTC. |
| `ordinal` | Long | Position in the article tree — sort by this to reproduce sidebar order. |
| `hasChildren` | Boolean | Cheap check for sub-articles; avoids pulling `childArticles`. |
| `hasStar` | Boolean | Whether the **current user** starred it — token-dependent, not global. |

**Entity / collection** (require parentheses):

| Selector | Type | Sub-selector catalog |
|---|---|---|
| `project(...)` | Project | §4.2 |
| `reporter(...)`, `updatedBy(...)` | User | §4.1 |
| `parentArticle(...)`, `childArticles(...)` | Article (recursive) | §5 |
| `comments(...)`, `pinnedComments(...)` | Array of ArticleComment | §4.3 |
| `attachments(...)` | Array of ArticleAttachment | §4.4 |
| `tags(...)` | Array of Tag | §4.5 |
| `visibility(...)` | Visibility | §4.6 |
| `externalArticle(...)` | ExternalArticle | §4.6 |

### Naming traps vs `Issue`

| Concept | `Issue` | `Article` |
|---|---|---|
| Title | `summary` | `summary` |
| Body | `description` | **`content`** |
| Rendered body | `wikifiedDescription` | **does not exist** |
| Last editor | `updater` | **`updatedBy`** |
| Hierarchy | `parent` / `subtasks` (IssueLink) | `parentArticle` / `childArticles` (Article) |
| Custom fields | `customFields` | **do not exist** |

Never emit `description`, `customFields`, `updater`, `votes`, `links`, `commentsCount` or
`resolved` for an article — none of them are `Article` attributes.

## 4. Nested selector catalogs

### 4.1 `User`

`id`, `login`, `fullName`, `email`, `ringId`, `guest`, `online`, `banned`, `banBadge`,
`banReason`, `isAnonymized`, `avatarUrl`, `tags(...)`, `savedQueries(...)`, `profiles(...)`,
`userType(...)`. Official article samples also use `name` for the display name
(`reporter(name)` → `"John Smith"`), inherited rather than listed in the `User` table.

Practical picks: `reporter(login,fullName)`, or `reporter(name)` to match the docs' own samples.

### 4.2 `Project`

`id`, `name`, `shortName`, `description`, `archived`, `template`, `iconUrl`, `startingNumber`,
`fromEmail`, `replyToEmail`, `leader(...)`, `createdBy(...)`, `customFields(...)`, `team(...)`,
`issues(...)`.

**Never request `project(issues(...))`** — that is every issue in the project, returned inside an
article payload.

Practical picks: `project(id,shortName,name)`.

### 4.3 `ArticleComment`

`id`, `text`, `created`, `updated`, `pinned`, `author(...)` (User),
`attachments(...)` (ArticleAttachment), `reactions(...)`, `visibility(...)`,
`article(...)` (back-reference — omit).

Note the difference from `IssueComment`: there is **no** `textPreview` and **no** `deleted`.

Practical picks: `comments(id,text,created,author(login,fullName),pinned)`.

### 4.4 `ArticleAttachment`

`id`, `name`, `size`, `extension`, `mimeType`, `charset`, `metaData`, `created`, `updated`,
`draft`, `removed`, `url`, `base64Content`, `author(...)` (User), `visibility(...)`,
`article(...)`, `comment(...)`.

`comment` is `null` when the file was attached to the article directly rather than to a comment.
There is **no** `thumbnailURL` here (unlike `IssueAttachment`).

**`base64Content` inlines the whole file** as a data URI — request it only when the bytes are the
goal. Prefer `url`.

Practical picks: `attachments(id,name,mimeType,size,url)`.

### 4.5 `Tag`

`id`, `name`, `color(...)`, `untagOnResolve`, `owner(...)`, `readSharingSettings(...)`,
`tagSharingSettings(...)`, `updateSharingSettings(...)`, `issues(...)`, plus deprecated
`visibleFor(...)` / `updateableBy(...)`.

**Never request `tags(issues(...))`.**

Practical picks: `tags(id,name)`.

### 4.6 `Visibility` and opaque wrappers

`Visibility` is polymorphic — request `$type` to tell restricted from unrestricted. The concrete
subtype's attribute names are **not covered by the sources used for this document**; verify them
against the target instance before relying on a spelling, and use `visibility($type)` when only
"is it restricted" matters. The same caution applies to `externalArticle`.

## 5. The tree: `parentArticle` and `childArticles`

Both are `Article`, so the sub-selection accepts every selector from §3 — including
`childArticles` again. **Recursion is the main failure mode here.**

- Depth is literal: `childArticles(childArticles(childArticles(id,summary)))` fetches three
  levels and nothing deeper. There is no "all descendants" selector.
- **Never** put `content` inside a `childArticles` sub-selection — that pulls whole document
  bodies for the entire subtree.
- **Never** nest `parentArticle(childArticles(...))` or `childArticles(parentArticle(...))` —
  each walks back into what you already have.
- Prefer flat reconstruction: request `id,idReadable,summary,ordinal,parentArticle(id),hasChildren`
  over the whole article list and rebuild the tree client-side. One flat pass beats N nested ones.
- Use `hasChildren` instead of requesting `childArticles(id)` just to test emptiness.
- Sort siblings by `ordinal`.

Safe one-level expansion:
```
id,idReadable,summary,ordinal,hasChildren,childArticles(id,idReadable,summary,ordinal)
```

## 6. Ready-made presets

| Task | `fields` |
|---|---|
| Bare reference | `id,idReadable` |
| Index / list of articles | `id,idReadable,summary,created,updated,project(shortName),reporter(login,fullName)` |
| Tree reconstruction (flat) | `id,idReadable,summary,ordinal,hasChildren,parentArticle(id)` |
| One level of children | `id,idReadable,summary,ordinal,childArticles(id,idReadable,summary,ordinal)` |
| Read one article | `id,idReadable,summary,content,created,updated,project(id,shortName),reporter(login,fullName),updatedBy(login,fullName),tags(id,name)` |
| Docs-sample equivalent | `hasStar,content,created,updated,id,idReadable,reporter(name),summary,project(shortName)` |
| Comment thread | `id,idReadable,comments(id,text,created,updated,author(login,fullName),pinned)` |
| Attachments | `id,idReadable,attachments(id,name,mimeType,size,url,created,author(login))` |
| Content export | `idReadable,summary,content,project(shortName),updated` |

## 7. Cost rules

1. **`content` never goes into a list request** unless the task is literally exporting bodies.
   Use `summary` for listings.
2. No recursion into `childArticles` beyond the depth actually needed; no `content` inside it.
3. No back-references: `project(issues(...))`, `tags(issues(...))`, `comments(article(...))`,
   `attachments(article(...))`, `parentArticle(childArticles(...))`.
4. `comments` and `attachments` belong in per-article detail requests, not in listings. There is
   no `commentsCount` shortcut on `Article` — if you only need to know whether comments exist,
   request `comments(id)` with pagination limited, and accept the cost.
5. `base64Content` inlines file bytes.
6. Collections cap at **42** entries without `$top` — a short list may be truncated. Paginate
   instead of widening `fields`.

## 8. Checklist before emitting `fields`

- Every entity-typed selector has parentheses; every scalar has none.
- Body selector is `content`, not `description`; last editor is `updatedBy`, not `updater`.
- No `customFields` — articles have none.
- No `content` inside `childArticles`; recursion depth is intentional and finite.
- No back-reference traversal.
- Timestamps (`created`, `updated`) are **milliseconds**.
- `hasStar` reflects the calling token's user, so do not present it as a property of the article."#;
