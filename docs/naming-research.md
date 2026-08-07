# Agent-Harness Naming Research

Name criterias ( weak filter ):

- female like
- pure ASCII chars
- two or three syllables ( less is better )
- area of themes
  - myth
  - fantasy
  - tolkien
  - sci-fi
  - philosophy
  - religious
  - tech-element
  - computing / software

## Research topics / backgrounds searched so far

- **Myth / fantasy / Tolkien / sci-fi**: Ylva, Nysa, Clotho, Eos, Sif, Runa, Eira, Vesta, Sera, Ayla, Thalia, Vixa, Nyra, Thebe, Cala, Nyx, Astra, Vela, Rhea, Jael, Petra, Anya, Sable, Cyra, Siren, Hexa, Lumen, Vera, Zetta. Plus IP-guarded (avoid): Lain, Quorra, Motoko.
- **Philosophy / religious / Christian**: Hagia, Alethea, Selah, Sophia, Zoe, Charis, Pistis, Elpis, Agape, Irene, Anima, Arete, Spes, Sancta, Kyria, Thecla, Eunoia, Domina, Fides, Caritas, Themis, Theoria, Perpetua, Andreia, Phronesis, Veritas, Lucia, Eden, Shalom, Ora, Dika, Doxa, Theia, Atropos, Philia, Gratia, Tirzah, Hosanna, Zephyra, Beata, Seraph, Redempta, Stella, Regina, Lachesis, Adah, Keziah, Zipporah, Miriam, Deborah, Ruth, Gloria, Pax, Amata, Ecclesia, Mnemosyne.
- **Tech / element / physical-science / oxygen-adjacent** : Yttria, Vanadis, Helia, Osmia, Erbia, Titania, Selenia, Galatea, Quanta, Lithia, Ceria, Thoria, Palladia, Ruthenia, Iridia, Zirconia, Galena, Ozona, Aethra, Dynamis, Silica, Mangana, Halite, Calcite, Manganite, Gravita, Cometa, Nebula, Terra, Aqua, Ignis, Aether, Oxygen, Oxy, Nova, Aurora, Electra, Plasma, Ion, Soda.

## Method + remaining checks

Candidate names are checked for existing usage / trademark risk using:

- `ddgr --json` (web search)
- `brew search` (Homebrew formulae/casks)
- npm registry, PyPI, GitHub API (deterministic checks)
  - **npm** (unscoped + scoped + org + user): `https://registry.npmjs.org/<name>` (package), `https://registry.npmjs.org/@<scope>%2F<name>` (scoped package), `https://registry.npmjs.org/-/org/<name>` (org scope `@<name>` — 404 "does not exist" = free), `https://registry.npmjs.org/-/user/<name>` (user handle). Cross-check with `npm view <name>` / `npm view @<scope>/<name>`. Note: `npmjs.com/org/<name>` and `/~<name>` are Cloudflare-gated and not reliable for scripted checks.
- **crates.io / lib.rs / docs.rs** (Rust crate, user handle, and docs namespace — via `https://crates.io/api/v1/crates/<name>` with a descriptive `User-Agent`; HTTP 404 = available, 200 = taken)
- **pkg.go.dev + proxy.golang.org** (Go module index — `https://pkg.go.dev/search?m=package&q=<name>` returns "Showing 0 modules" when free; `https://proxy.golang.org/<module>/@latest` → 404 for an unregistered path). Note: Go modules are path-keyed (e.g. `github.com/<user>/<repo>`), so the `github.com/<name>/<name>` repo check is the binding availability test.
- GitHub repo search (name in repo, top-starred)
- RDAP + WHOIS + DNS NS (domain registration)
- USPTO / EUIPO / TMview trademark lookups (via web searches + uspto.report / trademarkelite)

Remaining checks before finalizing:

1. Decide among **Ylva / Nerida / Hagia**: Ylva & Nerida are the safest on trademarks; Hagia is the only one with a free `.dev`/`.io`.
2. For the chosen name, confirm `github.com/<name>` handle/repo/org availability (only repo counts checked so far).
3. Optional USPTO/TSDR re-pull to confirm Class-5-only / non-software status.
4. Confirm availability on the exact package index you'll publish to.
5. Research the **PROPOSED · A / B / C** names below (unverified) using the same pass, since they have not yet been checked.
6. (Non-legal note) Research only, not legal advice.

Source provenance: domains via WHOIS / RDAP (rdap.org) / DNS NS (a returned-only-IANA + no NS = available); US trademark classes via uspto.report (e.g. /TM/98619402 = Intl Class 5); Elanor Labs via trademarkelite.

## Status legend

- **LOW** — available, low risk (top pick)
- **LOW-MED** — available with caveat (shortlist)
- **MED** — usable but flagged
- **RISK** — existing product/trademark / notable collision
- **TAKEN** — registry/domain taken
- **DROPPED** — research reason (taken / IP / poor fit)
- **PROPOSED · A / B / C** — unverified; still needs the full pass (A = research next, B = flag collisions, C = avoid)
- **REJECTED** - rejected by user

## Candidate Tier 1 — PRIMARY (LOW)

### Ylva — LOW

- Category: LOW → current top pick (edges Nerida on collision)
- Meaning: Old Norse **"she-wolf"** — 2 syllables, female, ASCII-clean, mythic.
- brew: none · npm: **free** — unscoped `ylva` → 404 ("Not found"); scoped `@ylva/ylva` → 404; **org scope `@ylva`** free (`/-/org/ylva` → 404 "does not exist"); **user handle `ylva`** free (`/-/user/ylva` → 404); `scope:ylva` search → 0 results. · PyPI: **404 (free)** · **crates.io: free** (`GET /api/v1/crates/ylva` → 404 "crate `ylva` does not exist"; user handle `crates.io/users/ylva` → 404 free; `lib.rs/crates/ylva` → 404 free; `docs.rs/ylva` → 404 free). Search for `ylva` on crates.io returns only substring matches (`sylvan_number`, `…_sylvania…`) — no exact crate. · **Go: free** (`pkg.go.dev/search?m=package&q=ylva` → "Showing 0 modules with matching packages"; `pkg.go.dev/ylva` and `pkg.go.dev/github.com/ylva/ylva` → 404; `proxy.golang.org/github.com/ylva/ylva/@latest` → 404). Module path `github.com/ylva/ylva` is obtainable since the `ylva` org and `ylva/ylva` repo are free (see GitHub line).
- GitHub: 59 repos (low). Handle `github.com/ylva` → HTTP 200; `/orgs/ylva` → 404 (org free); `github.com/ylva/ylva` repo → 404 (free). Resolved: `GET /users/ylva` → personal `User`, created 2010-07-26, last updated 2025-01-10, `public_repos: 2`, `followers: 2`, holding only trivial repos (`ylva/KitchenSink` — Titanium mobile demo; `ylva/phonegap-plugin-facebook-connect`). A long-idle, low-value personal handle. **Conclusion:** the `ylva` **org** (natural home for a harness) **can be created** and `ylva/ylva` repo is claimable; the bare user handle is occupied but non-conflicting.
- Docker / ghcr: Docker Hub account/namespace `ylva` **exists** (individual user, joined 2018-12-13, not a company) → the bare `ylva` Docker login is taken. But Docker Hub repo `ylva/ylva` → **404 (free)**, and the search (843 hits) has **no** exact `ylva/…` image (all prefixed: `sylva/sylva`, `ylvawr/…`, `ylvali/…`, `ylvag/…`). `ghcr.io`: anonymous token for `ylva/ylva` returns the same `DENIED` as a known-nonexistent baseline (no reliable anon test), but GHCR paths hang off the GitHub namespace and the `ylva` **org** is free → `ghcr.io/ylva/...` **obtainable**. Net: use `ylva/ylva` repo or `ghcr.io/ylva/` (both free); only the Docker login itself is unavailable.
- Homebrew: `github.com/homebrew-ylva` (tap `homebrew-ylva`) → **404 (free)**; `formulae.brew.sh/api/formula.json` scan → **no `ylva` match** (no core formula); cask `ylva.json` → no exact cask. A new tap can register as `homebrew-ylva`.
- Package indexes (reconfirmed): PyPI `ylva` → **404 (free)**; Maven Central group `io.github.ylva` → **numFound 0**; `dev.ylva` → **numFound 0**. Both Maven group coords free (npm/crates/Go already free, see above).
- Wikidata: primary-topic item **Q1760268 = "Ylva — female given name"** confirmed. **No** "Ylva (software)" / "Ylva (company)" item exists — nearest non-person entities are benign (`Q12344226` sailboat class; `Q83590200` ship; `Q67201141` *Vicky the Viking* character). No disambiguation shadow.
- Domains: `ylva.dev`, `ylva.io` → both **registered**
- Trademark: US **YLVA BLENDS CRAFTED FOR WOMEN** (99141479, Ylva Blends LLC) → **International Class 5** (dietary supplements, lion's mane, women's hormonal health). **Not software.** Also EU "YLVA" / "Ylva Li" marks in cosmetic/supplement classes.
- Assessment: free on both registries, low GH noise, trademark only in supplements class. Catch: `.dev`/`.io` taken.

#### Background / usage (justification for "Ylva")

- **Etymology & root**: feminine of the Old Norse / Old Swedish masculine name **Ulf**, from Proto-Germanic **`*wulfaz`** → Old Norse **`úlfr`** "wolf", with the feminine suffix **`-a`**. Literally "she-wolf". Sources: Swedish Academy almanackan, Historiska museet (Statens historiska museer), Behind the Name, Wiktionary.
- **Living, established given name** — not a coined/made-up coinage: a Swedish female given name with a **name day (April 11 in Sweden/Norway; Sept 13 in Swedish-speaking Finland)** and Wikipedia notability page. Also used in Norway, Denmark, Iceland, and Finland. This makes the name culturally grounded and pronounceable in the Nordic region, reducing "made-up brand" risk.
- **Thematic fit for an agent-harness**:
  - Wolves in Norse symbolism connote strength, intelligence, pack loyalty, and ferocity — the harness is the loyal pack-coordinator that channels an agent's tool calls.
  - "She-wolf" specifically evokes the protective/nurturing guide archetype (cf. the she-wolf suckling Romulus & Remus in Roman myth), reinforcing the "harness that tames the wild model" metaphor.
  - Feminine-form naming mirrors the existing female-name convention used in this research (Nerida, Hagia, Tirzah, etc.) and matches the "female-like" filter.
- **Notable real bearers** (demonstrates it is an authentic, low-controversy personal name, not a sensitive figure):
  - **Ingrid Ylva** (c. 1180s–1250s) — Swedish noblewoman, folk-historical figure.
  - Ylva Eggehorn (b. 1950, Swedish poet/writer), Ylva Johansson (b. 1964, Swedish politician/EU commissioner), Ylva Lindberg (ice-hockey), Ylva Nowén (alpine skiing), Ylva Thörn (politician), Ylva Ivarsson (archer), Ylva Lööf (actress).
- **Fictional appearances** (low-IP-risk, public-domain or generic usage — no single trademark owner):
  - Mother of Vicke Viking (children's book/TV character).
  - Ylva Haraldsdotter in Frans Gunnar Bengtsson's *Röde Orm / The Long Ships* (1941 novel, public-domain in many jurisdictions).
  - A character in *Vinland Saga* (manga) — name only, not a title/brand.
  - A plot device in the 2003 film *Alex & Emma* (characters debate whether "Ylva" is a real name — culturally confirms it is real and rare enough to be distinctive).
- **Other "Ylva" usages** (non-software, non-blocking):
  - **Ylva (keelboat)** — a 1973 Danish 12.68 m sailboat class designed by Steen Kjølhede (~100 built). Maritime, unrelated to software/agents; no trademark conflict with Class 9/42.
  - The name is rare in the US (≈35–500 bearers depending on source; SSA rank ≈ #15,163 in 2024, peak 7 births/yr in 2017) → distinctive in an English-speaking tech context without being unpronounceable.
- **Why this beats shortlist peers on the "background" axis**:
  - vs **Nerida** (LOW-MED): Ylva has a richer attested historical+literary record (Wikipedia given-name page + nameday + medieval noblewoman) and tighter thematic coupling to "agent-harness" (she-wolf = guided ferocity) than the generic "sea-nymph" Nerida.
  - vs **Hagia** (LOW-MED): Hagia's only AI namesake ("Hagia Labs", hagiaproject.com) is a direct same-field collision; Ylva has **no** AI/agent/software namesake found in ddgr or on Wikipedia.
  - vs **Tirzah** (LOW): Tirzah is also clean, but tied to a specific Hebrew-Bible/city identity and a UK musician; Ylva's mythic-wolf framing maps better onto a tech-agent persona.
- **Net**: Real Nordic given name with medieval attestation, name day, literary pedigree, and a thematically apt "she-wolf" meaning — at no IP cost in the software classes. The only real cost is the two taken gTLDs (`.dev`/`.io`), which is a registrar problem, not a legal or naming-fit problem.

#### Remaining checks to confirm Ylva (before finalizing)

Resolved and moved into the entry above: GitHub handle/org, package indexes (PyPI + Maven), Docker/ghcr, Homebrew, Wikidata (all DONE). Two live trademark pulls remain, both gated to a manual browser check:

1. **BLOCKED — USPTO TSDR live for Serial 99141479 (YLVA BLENDS).** uspto.report `/TSM/99141479`, trademarkia, and justia all return Cloudflare/managed-challenge (403) to scripted clients; no anonymous TSDR endpoint is available. Prior snapshot stands (Intl Class 5, Ylva Blends LLC, supplements). **Manual browser confirmation required**: reopen `uspto.report/TM/99141479` (or TSDR) and record LIVE status + class list.
2. **BLOCKED — EUIPO / TMview for "YLVA" + "Ylva Li".** `tmdn.org/tmview` and `euipo.europa.eu/copla` are both unreachable from this environment (`HTTP 000`). **Manual browser confirmation required** that the EU "YLVA" / "Ylva Li" marks are also outside Intl Class 9/42.

## Candidate Tier 2 — SHORTLIST (LOW-MED)

### Nerida — LOW-MED → best pick (small caveat: namesake trademark in other class)

- Meaning: Nereids (Greek sea nymphs) / Australian given name.
- brew: only fuzzy · npm: free · PyPI: free
- GitHub: `github.com/nerida`, `github.com/org/nerida`, `github.com/nerida/repos` → all **HTTP 404 (available)**. (A separate personal repo `nerida97/nerida` exists, unrelated.)
- Domains:
  - **nerida.dev → AVAILABLE** (no NS records; definitive)
  - nerida.io → registered · nerida.ai → registered (parked) · nerida.app → registered (+1 other)
- Trademarks:
  - US **NERIDA** (Serial 98619402, Nerida Europe Ltd) → **International Class 5** only (dietary supplements, vitamins, collagen, cosmetics). **Not software/dev.** Registration is LIVE/Pending (filed 2024). (via uspto.report/TM/98619402)
  - EU **"Nerida Minerals"** → minerals, unrelated. Lithuanian **Nerida UAB** → unrelated category.
  - **No found mark in software classes (9/42).**
- ddgr web: **Nereids** Greek sea-nymphs (Wikipedia) + established **Australian given name** with several notable bearers (Nerida Ellerton — mathematics educator/historian; Nerida Newton — author; Nerida Tyson-Chew — composer; Nerida Stewart — netball coach; Nerida Gregory — tennis; Nerida Wilson — marine). Non-dev brands: **neridaofficial.co.uk** (size-inclusive clothing), **neridaworld.com** (Athens clothing brand, est. 2022), **nerida.info** (Nerida Cuddy, singer-songwriter), and a French rock band "Nerida". **No tech/agent/harness namesake** in any ddgr hit (site:github results are pure search-noise — the one AI-adjacent hit `gaonerida-bit/Econometrics-AI-Research-Assistant` is an incidental substring, not a "Nerida" product).
- Source: RDAP/WHOIS/DNS NS — `nerida.dev` returned only IANA registry data + no NS = available.
- Assessment: cleanest in dev space. Caveats: (1) Class-5-only US mark (unlikely to block software/open-source); (2) `.io/.ai/.app` taken — **`nerida.dev` is the free one**.

### Nysa — LOW-MED

- Meaning: Greek myth (nymph / where Dionysus was raised); also an asteroid. 2 syll, female-ish.
- brew: none · npm: **free** · PyPI: **404 (free)**
- GitHub: 420 repos (higher than Ylva — named asteroid & beach resort dominate)
- Domains: nysa.dev, nysa.io → registered
- Trademark: **NYSA** (Reg 3282544) + **NYSA APPAREL** (99690094) + WIPO NYSA → **apparel/class 25**, **not software**. Also justia NYSA "US Class 25" apparel.
- Assessment: registry-clean, apparel trademarks only; more GH collision than Ylva.

### Hagia — LOW-MED (registry + domains clean, but an AI-labs namesake)

- Meaning: Greek **"holy / saint"** (as in Hagia Sophia). 2 syll, female-adjacent, ASCII-clean.
- brew: none · npm: **free** · PyPI: **404 (free)**; **hagia.dev AND hagia.io both AVAILABLE**
- GitHub: 203 repos (moderate)
- Trademarks: **HAGIA SOPHIA** (Saint Olga Russian Orthodox) + HAGIA HER; "SAFESAVE" is the Hagia Sophia LLC mark. Religious/architectural context, not software.
- Caveat: **Hagia Labs** (hagiaproject.com) is an active AI-creativity company — close, but not the exact standalone word.
- Assessment: strongest registry+domain story in the religion background (both `.dev`/`.io` free); the Hagia Labs AI collision is the caveat.

### Alethea — LOW-MED (AI company namesake)

- Meaning: Greek **"truth"**; 3 syll (slightly long), female.
- brew: none · npm: **free** · PyPI: **404 (free)**; alethea.dev/.io taken
- GitHub: 154 repos
- Trademarks: ALETHEA by Macy's IP Holdings (97524444), ALETHEA LEGAL, Adham Bedir (99110480), an EU mark — multiple owners.
- Caveat: **alethea.com is a real AI company** (disinformation/AI-safety detection) → risky despite free registries.

### Selah — LOW-MED (registry clean, but high GH + religious trademark cluster)

- Meaning: Hebrew Psalms pause word ("selah"). 2 syll, ASCII.
- brew: none · npm: **free** · PyPI: **404 (free)**; selah.dev/.io taken
- GitHub: 768 repos (high)
- Trademarks: SELAH BRAND, SELAH Beverage Corp, multiple SELAH marks — crowded, though mostly beverage/apparel/religion, not software.
- Assessment: registry-clean but heavy GH noise + many marks. Worse than Ylva/Nysa.

## Candidate Tier 3 — SECONDARY (MED)

### Clotho — MED (weaver-on-theme, but taken)

- Meaning: the Greek Fate who **spins the thread** — thematically perfect for an agent-harness (weaves tool calls).
- brew: none · npm: **taken** · PyPI: **taken** (200) · GitHub: 371 repos
- Assessment: registry collision. Runner-up only.

### Elanor — MED → downgraded (new trademark evidence)

- Meaning: Tolkien-adjacent flower (elanor); 3 syll, female.
- brew: no match · npm: free · PyPI: free
- GitHub: `elanor` handle / repo / org → all HTTP 404 (available)
- Domains: elanor.dev, elanor.io, elanor.ai → all **registered**; elanor.app → available
- Trademarks / IP:
  - **Elanor Labs Inc.** (Indianapolis) — "REVENUE BEHAVIORAL INTELLIGENCE" + "REVENUE BEHAVIORAL INDEX" (2026). Company carries the "Elanor" name (software/analytics-adjacent). (source: trademarkelite)
  - **"Eleanor" Mustang** — high-profile, litigated (Shelby Trust). "Elanor" is one letter away in the same word family. **ELEANOR** trademark (Reg 2324306) exists.
- GitHub API: 113 repos incl. a static site generator (syswraith/elanor) and an Ollama web client ("elanore").
- Assessment: despite free registries, riskier now (real "Elanor Labs" software company + "Eleanor" trademark). Recommend against.

## Candidate Tier 4 — RISK (researched, avoid)

### Vixa — RISK (despite free registries)

- brew: none · npm: **free** · PyPI: **404 (free)**; `vixa.dev` **AVAILABLE**
- GitHub: 211 repos; incl. `vixa-ai` org, `bjornmoggy001/Vixa` (AI video-editing SaaS), `Usevixa/vixa-ai-landing-page`
- Trademarks: **VIXA** by Vixa LLC (99864843), Tdsl Enterprises (90094842), Xiamen Baola (87406139)
- Web: "Vixa from The AA" (car-health app), Vixa Motors, Vixalabs.com (AI video), Vixa Watches, fleet/vehicle-health SaaS.
- Assessment: registry-clean + free `.dev`, but active tech/AI usage (Vixalabs, vixa-ai, Vixa LLC, The AA). Avoid.

### Noira — RISK (MED)

- Free on registries but widely used: jewellery (noiraonline.com), wellness (getnoira.com), a VTuber (noira.me), GitHub projects (noira-player, Noira OS, NoiraResearchLab "Noira AI core"). Avoid for clarity.
- ddgr web: jewellery, wellness ("NOIRA lymphatic drainage / fulvic acid"), a sea-fox VTuber (noira.me), a wine bar (Salem OR), plus the **NOIRA / Notice of Intended Regulatory Action** government acronym (Virginia Regulatory Town Hall) — a crowded, multi-domain name. `site:github` confirms tech noise (`noira-player` open-source TV media player, `NoiraResearchLab/Noira-` "AI core" blockchain, `noira-os` macOS desktop, `noira-crm`) — unanimous recommend-avoid.

### Aelwen — RISK (MED)

- **Oggure/Aelwen-Shell** (GPT4All-based AI shell) + **aelwen.com** (Hangzhou Auto Parts company). Avoid.
- ddgr web: otherwise a genuine Welsh given name meaning **"white brow / fair brow"** (welshgirlsnames, nameberry, BabyNames, Behind the Name) — i.e. the name itself is low-collision; the blockers are purely the two commercial/software namesakes above (`site:github` confirms Oggure/Aelwen-Shell; the rest is wordlist/password search noise).

### Nimloth — RISK (MED)

- Strong Tolkien IP (Estate / Middle-earth Enterprises) + existing `nimloth` / `nimloth-a` GitHub orgs + active react-native project. Avoid.
- ddgr web: dominated by Tolkien entities — **Nimloth the White Tree of Númenor** and **Nimloth the Elf of Doriath** (Tolkien Gateway, lotr.fandom, lotro-wiki, Encyclopedia of Arda). A software collision: **thenimloth.com**, "Nimloth", a decision-support system for hiring/recruitment. `site:github` adds `github.com/nimloth` user (14 repos), `TheNimloth/nimloth-site`, `nimloth-react-native` by two authors, and a `nimloth-a` org — all active. Double blocker: Tolkien IP + an active recruitment product.

### Ondine — RISK (avoid)

- PyPI taken by "ondine" (batch-process DataFrames with LLMs) — direct domain collision. Avoid.

## Candidate Tier 5 — TAKEN (researched)

### Moira — TAKEN

npm + PyPI taken; 1062 GitHub repos incl. moira-alert (320★). Greek "fate", lovely but used.

### Eos — TAKEN

npm/PyPI taken (`eos`); 200 GH; many products (EOS blockchain, Canon EOS). Avoid.

### Sif — TAKEN

Norse mythic female; npm taken. Doesn't beat Ylva/Nysa.

### Runa — TAKEN

npm free but PyPI taken.

### Eira — TAKEN

Norse/welsh mythic female; npm taken.

### Vesta — TAKEN

Roman goddess; npm taken.

### Sera — TAKEN

npm taken.

### Ayla — TAKEN

npm taken.

### Thalia — TAKEN

Greek Muse; npm taken.

### Nyx — TAKEN

Greek night; both registries + 200 GH.

### Astra — TAKEN

npm taken.

### Vela — TAKEN

constellation; both taken, kubevela etc.

### Rhea — TAKEN

Titaness; both taken.

### Jael — TAKEN

npm taken.

### Petra — TAKEN

npm taken.

### Anya — TAKEN

npm taken.

### Sable — TAKEN

both taken.

### Cyra — TAKEN

cyber-flavored female; npm taken.

### Siren — TAKEN

npm taken.

### Hexa — TAKEN

npm taken.

### Lumen — TAKEN

npm taken (PyPI 200).

### Vera — TAKEN

npm taken (PyPI 200).

### Zetta — TAKEN

npm taken (PyPI 200).

### Sophia — TAKEN

Greek "wisdom"; npm taken.

### Zoe — TAKEN

Greek "life"; both taken, 283 revs npm.

### Charis — TAKEN

Greek "grace"; npm taken.

### Pistis — TAKEN

Greek "faith"; npm taken.

### Elpis — TAKEN

Greek "hope"; npm taken.

### Agape — TAKEN

Greek "love"; npm taken (py free).

### Irene — TAKEN

Greek "peace"; npm taken.

### Anima — TAKEN

Latin "soul"; npm taken.

## Candidate Tier 6 — PROPOSED · A (researched)

### Tirzah — LOW (**top new pick**)

- Meaning: Hebrew OT name (daughters of Zelophehad) + city in Israel; also _"delight"_. 2 syll, fem, ASCII.
- brew: none · npm: **free** · PyPI: **404 (free)**
- GitHub: 47 repos (very low); handle `github.com/tirzah` taken (personal account, minor)
- Domains: **tirzah.dev / .io / .app / .ai → ALL AVAILABLE** (no NS records)
- Web: Hebrew Bible name + a UK musician; **no tech/agent/harness namesake**
- Assessment: registries free, very low GH noise, **all four TLDs free**. Best new Tier-A candidate — rivals Ylva/Nerida.

### Erbia — LOW

- Meaning: **erbium oxide** (Ytterby element family). 3 syll, fem, rare.
- brew: none · npm: **free** · PyPI: **404 (free)**
- GitHub: 14 repos (very low)
- Domains: erbia.dev / .app / .ai → **AVAILABLE**; erbia.io → registered (one.com)
- Web: no notable results (clean)
- Assessment: ultra-rare, registries free, low GH; only `.io` taken.

### Mangana — LOW (Tier-B set, promoted)

- Meaning: from **manganese / magnes "magnet"**. 3 syll, fem.
- brew: none · npm: **free** · PyPI: **404 (free)**
- GitHub: 140 repos (low)
- Domains: **mangana.dev / .io / .app / .ai → ALL AVAILABLE**
- Web: Byzantine district, a Tasmania locality, and near-"manga" (manga sites) — **no tech/agent namesake**
- Assessment: registries free + all four TLDs free + low GH. Caveat: spelling sits near "manga".

### Erla — MED

- Meaning: fem of **Erlang** (Erlang the engineer); also Norse given name. 2 syll.
- npm: free · PyPI: free · GitHub: 11k repos (substring noise) · handle taken
- Domains: only erla.io → AVAILABLE (.dev/.app/.ai registered)
- Assessment: registries free but heavy GH noise + only one TLD free.

### Vanadis — MED

- Meaning: Norse goddess Freyja → **vanadium**. 3 syll.
- npm: free · PyPI: **taken** (200) · GitHub: 35 (low)
- Assessment: story is great but **PyPI collision** blocks it.

### Radia — MED

- Meaning: **Radia Perlman** ("mother of the internet"). 3 syll, fem.
- npm: free · PyPI: **taken** (200) · GitHub: 16.8k (substring noise)
- Assessment: strong person-origin but **PyPI taken** + GH noise.

### Arete — TAKEN

npm TAKEN · PyPI TAKEN. Greek "excellence/virtue" (fem). 2 syll.

### Theia — TAKEN

npm TAKEN · PyPI TAKEN. Greek Titan of sight/light. 2 syll.

### Philia — TAKEN

npm TAKEN. Greek "friendship/affection" (Aristotle). 3 syll.

### Helia — TAKEN

npm TAKEN. Helios→helium family, given name. 3 syll.

### Galena — TAKEN

npm TAKEN · PyPI TAKEN. lead-sulfide mineral (PbS). 3 syll.

### Dynamis — TAKEN

npm TAKEN. Greek "power/energy" (dynamic). 3 syll.

### Silica — TAKEN

npm TAKEN · PyPI TAKEN. silicon dioxide. 3 syll.

## Candidate Tier 7 — PROPOSED · B (researched)

### Palladia — LOW-MED

- Meaning: **palladium**. 4 syll (bit long).
- brew: none · npm: **free** · PyPI: **404 (free)** · GitHub: 19 (very low)
- Domains: palladia.dev / .app → **AVAILABLE**; .io/.ai registered
- Assessment: ultra-rare, registries free; 4 syll is the only knock.

### Ceria — MED

- Meaning: **cerium oxide**. 3 syll.
- npm: free · PyPI: free · GitHub: 808 (moderate)
- Domains: only ceria.dev → AVAILABLE
- Assessment: moderate GH + domain-poor.

### Thoria — MED

- Meaning: **thorium oxide**. 3 syll.
- npm: free · PyPI: free · GitHub: 26 (low)
- Domains: only thoria.io → AVAILABLE
- Assessment: clean registry but domain-poor.

### Perpetua — TAKEN

npm TAKEN · PyPI TAKEN. early Christian martyr. 4 syll.

### Calcite — TAKEN

npm TAKEN · PyPI TAKEN. calcium carbonate mineral. 3 syll.

## Candidate Tier 8 — REJECTED (by user)

User-rejected candidates consolidated from the previous PROPOSED A/B/C pools. Reasons kept from their prior entries.

**Religious / philosophy (A-pool):** Spes (Latin "hope", 1 syll), Sancta ("holy/saint"), Kyria (Greek "Lady"), Thecla (saint/martyr), Eunoia ("beautiful thinking"; niche CSS engine), Domina ("lady"), Atropos (Fate who cuts thread), Gratia ("grace"), Hosanna (acclamation).

**Element / mineral / science (A-pool):** Yttria (yttrium oxide), Osmia (osmium; mason-bee genus), Titania (titanium+fairy queen; Titania security product), Ozona (ozone; Texas town), Aethra (bright upper air, 2 syll).

**Computing / software (A-pool):** Pythia (Python+oracle), Sema (semaphore), Automa (automata/automation), Vala (Vala lang), Elixa (Elixir fem), Peta (SI prefix), Exa (SI prefix), Arachne (Greek weaver).

**Brand/product/legal collision (B-pool):** Fides (Ethyca privacy engine), Caritas (Caritas Intl), Themis (legal/fintech), Andreia (courage), Phronesis (prudence), Zephyra (west wind), Beata (SaaS), Seraph (SERAPH GeForce), Redempta (hard to pronounce), Stella (common), Regina (common), Lachesis (taken-like thread weaver), Adah/Keziah/Zipporah (OT, moderate), Selenia (selenium namesake), Galatea (taken), Quanta (Quanta Computer/Magazine), Lithia (Lithia Motors).

**Element/long (B-pool):** Ruthenia (ruthenium), Iridia (4 syll long), Zirconia (4 syll long), Halite (rock salt), Manganite (manganese ore), Gravita (gravity prior art), Cometa (comet prior art), Nebula (many using), Terra (Terraform), Aqua (crowded), Ignis (fire), Aether (fifth element, masc).

**Computing/software (B-pool):** Rosetta (Apple/Rosetta Stone), Frida (Frida framework + Kahlo), Emmy (Emmy awards), Rexa (REXX fem), Ziga (Zig fem / Slavic name).

**C-pool — already risky/avoid:** Veritas, Lucia, Eden, Shalom, Ora, Dika, Doxa, Sophrosyne, Dikaiosyne, Eudaimonia, Episteme, Miriam, Deborah, Ruth, Gloria, Pax, Amata, Ecclesia, Mnemosyne, Oxygen, Oxy, Nova, Aurora, Electra, Plasma, Ion, Soda, Vitra, Vectra, Peroxide, Osmium, Alumina, Phosphora, Oxyra, Ossia, Oxygêna (non-ASCII), Aetherea, Ozelle, Julia, Ada, Grace, Hypatia, Rosalind.

## Candidate Tier 9 — DROPPED (researched, avoid with reason)

### Nyra — DROPPED

npm taken · PyPI taken · 676 GH repos.

### Thebe — DROPPED

npm taken · PyPI taken · 5693 GH repos (heavy).

### Cala — DROPPED

npm taken · PyPI taken · 15k GH repos.

### Lain — DROPPED

hacker-anime character — copyrighted IP.

### Quorra — DROPPED

Tron female program — copyrighted IP.

### Motoko — DROPPED

Ghost in the Shell character — copyrighted IP.

## Proposed further checks (to improve research quality)

These are general methodology improvements that would harden the research for any future candidate.

### A. Methodology hardening (applies to all candidates)

1. **Snapshot provenance with timestamps.** Each registry/trademark result is point-in-time; add `date checked` ISO timestamps to every row so a future re-audit can tell staleness from drift. Right now only some rows have implicit dates.
2. **Standardize the trademark class verdict.** Always record: (a) the *exact* Nice International Class numbers, (b) LIVE/ABANDONED/PENDING status, (c) owner, (d) goods description. "Not software" is too loose; record Class 9 (electrical/computer) and Class 42 (SaaS/scientific) explicitly as "absent".
3. **Cross-jurisdiction trademark sweep**, not just US. For each top pick, check at minimum **US (USPTO), EU (EUIPO/TMview), UK (UKIPO), WIPO (Madrid)**. Class 5 in the US does not preclude a Class 9/42 mark elsewhere.
4. **Common-law / unregistered mark check.** Search for the name + "software" / "AI" / "agent" / "harness" via web search and GitHub READMEs to catch *unregistered* but commercially used marks (e.g., the Hagia Labs case was caught only because of web search, not the trademark DB).
5. **Pronunciation & transliteration check.** For an international open-source project, verify the name is pronounceable and unambiguous in ASCII (no diacritic fallback surprises). Ylva passes, but record the reasoning; some candidates (e.g., `Lööf`) lose diacritics.
6. **Exact-match vs fuzzy noise separation.** GitHub repo counts currently mix substring hits. Re-run with `gh search repos ylva in:name --exact` (or API `q=ylva in:name`) and separately record substring noise — gives a cleaner "name-collision" signal.
7. **Social-handle availability** on the channels the project will actually use (X/Twitter, Mastodon, HN, LinkedIn, PyCon/SIG listings). These are cheap to check and often the first thing that blocks a project rebrand.
8. **Domain strategy fallback.** When `.dev`/`.io` are taken, enumerate and price alternates the project would accept (`.app`, `.ai`, `.so`, `.run`, `.sh`, `get<name>.dev`, `<name>.agent`, `<name>harness.dev`) so a taken gTLD doesn't derail an otherwise clean pick.
9. **Disambiguation-page check.** Pull the candidate's Wikipedia disambiguation page (if any) — names with a disambiguation page (e.g., "Moira", "Eos", "Petra") tend to collide more; absence of one (Ylva has none) is a positive signal.
10. **Acronym / homoglyph collision.** Search USPTO/Google for the all-caps form `YLVA`, leetspeak variants (`Y1va`, `Ylva`), and confusable lookalikes (Punycode/IDN homoglyphs) to catch phishing/impersonation surface.
11. **Search-volume / SEO baseline.** `ddgr "Ylva"` result count and top-10 domains give a baseline for how much the project will have to "out-rank" to be discoverable. Ylva's top results are baby-name sites and a Wikipedia given-name page — low-competition, easy to outrank.
12. **Cultural-sensitivity sweep.** For names from living traditions (Norse, Hebrew, Greek), do a quick pass for any pejorative or sacred-only connotation that would make tech use inappropriate. Ylva is benign ("she-wolf" given name, name day); confirm and record.
13. **Reproducibility script.** Wrap the deterministic checks (npm, PyPI, GitHub API, RDAP/DNS, Docker Hub, crates.io) into a single shell script that emits a JSON/markdown row per candidate, so the matrix can be re-run after each new candidate is proposed and the doc regenerated from output rather than hand-edited. This is the single biggest quality lever: it removes transcription error and makes the audit repeatable.
