# Searchcord Bauhaus web direction

Status: approved home promoted to production; secondary designs remain local drafts

Working preferences: read [BAUHAUS-DESIGN-PREFERENCES.md](BAUHAUS-DESIGN-PREFERENCES.md) before making further visual changes.

## Recommendation

Use Bauhaus as a layout and information-architecture system, not as a retro color costume. The strongest fit for Searchcord is an editorial archive: a stable index, explicit rules, large numerical hierarchy, modular panels, and a search result field that feels composed rather than terminal-like.

Keep the existing FastAPI endpoints and client behavior out of this experiment. First validate the visual language with static mock data, then promote only the parts that improve browse, search, live monitoring, and stats without making the product harder to operate.

## What the research actually supports

The Dribbble search page is an inspiration index, not a single canonical Bauhaus system. The representative shots repeat a few formal moves:

- **Bauhaus Legacy** uses a reduced, mostly monochrome field, large geometric planes, strong cropping, and transitions that treat the page as a composition.
- **Bauhaus FC** makes the grid and type do most of the work: condensed sans-serif labels, hard edges, asymmetric blocks, and a limited accent palette. Its palette is documented on the shot, but that palette is not required for the structure.
- **Bauhaus Landing Page** and **Bauhaus Website** use modular rectangles, circles, and oversized type to create hierarchy and movement without ornamental UI chrome.

The historical references are more precise than the label “Bauhaus style”:

- MoMA describes Bauhaus work as simple, balanced geometries used in service of functional efficiency.
- MoMA’s account of the Bauhaus web concept identifies the grid as the organizing element and calls out clean lines plus a stable grid while content is sorted dynamically.
- Herbert Bayer’s new typography is described as sans-serif, grid-based, asymmetrical, and aimed at clarity. The relevant idea for Searchcord is the ordering system, not copying a historical alphabet.
- The New Typography reference supports treating the page as an open field where blocks of type and imagery can be arranged asymmetrically, instead of defaulting to centered symmetrical columns.

## Bauhaus form research: blue-banner repair

The banner geometry is grounded in the Getty Research Institute’s Bauhaus teaching materials, not a generic “retro geometry” moodboard:

- The Preliminary Course worked with points, lines, planes, circles, squares, triangles, and color as foundational material. These were investigated through exercises rather than treated as a fixed decorative kit.
- Form studies explored the different character of each shape. Subdividing and arranging forms was used to study proportion, contrast, visual tension, and rhythm.
- Exercises arrayed colored cutouts on grids and within squares; analytical drawing examined axes and linear tensions. Interaction between elements and their placement mattered as much as the isolated shape.
- Color associations were not a universal rule. Bauhaus teachers held different theories of color, so Searchcord should not imply that a circle, square, or triangle has one historically mandatory color.

Sources: [Getty Research Institute, Form and Color](https://www.getty.edu/research/exhibitions_events/exhibitions/bauhaus/new_artist/form_color/form/); [Getty Research Institute, Bauhaus Beginnings wall text](https://www.getty.edu/research/exhibitions_events/exhibitions/bauhaus/beginnings/wall_text.pdf); [Getty Research Institute, Principles and Curriculum](https://www.getty.edu/research/exhibitions_events/exhibitions/bauhaus/new_artist/history/principles_curriculum/); [Bauhaus Dessau Foundation, The Power of Color and Form](https://artsandculture.google.com/story/the-power-of-color-and-form-at-bauhaus-stiftung-bauhaus-dessau/XAXRMKlYpAG6Ig?hl=en).

Banner translation: keep the field open, remove the orthogonal grid, and use five angled rules across the banner plus two through the lower-left group. Remove isolated mini ornaments. Compose large circles and triangles in right, left, and center groupings, with the square and bar supporting the right group. Keep the lower-left circle-and-triangle group tilted and irregular. Let the geometry sit behind the search copy and filters with a visible banner-colored clearance; rules appear to stop at each clearance and resume after it. Keep every circle whole. When filters expand, retain the full right-side group and shift it upward on wider layouts; on narrow screens keep its large circle and triangle visible while simplifying the other groups. Use deep navy, light lavender, pale blue, and mint green as contemporary choices, not as claimed historical color mappings; avoid yellow and warm accents.

Interaction repair: keep the search interaction reversible. On open, lift the main right group clear of the filter tray before the tray unfolds; on close, retract the tray before the group returns. Outside click or Escape restores the full resting composition. Keep the motion tied to the user action, preserve reduced-motion behavior, and do not add perpetual animation.

Sources:

- [Dribbble: Bauhaus website search](https://dribbble.com/search/bauhaus-website)
- [Bauhaus Legacy website design](https://dribbble.com/shots/24035833-Bauhaus-Legacy-website-design)
- [Bauhaus FC Website concept](https://dribbble.com/shots/5306972-Bauhaus-FC-Website-concept)
- [Bauhaus Landing Page](https://dribbble.com/shots/15827382-Bauhaus-Landing-Page)
- [MoMA: Bauhaus and Beyond](https://www.moma.org/calendar/galleries/5388)
- [MoMA: Bauhaus, from Weimar to the Web](https://www.moma.org/explore/inside_out/2009/11/13/bauhaus-from-weimar-to-the-web/)
- [MoMA: Herbert Bayer, Architektur Lichtbilder Vortrag](https://www.moma.org/collection/works/5101)
- [MoMA: The New Typography](https://www.moma.org/calendar/exhibitions/1013)

## Translation into Searchcord

| Bauhaus principle | Searchcord application | Guardrail |
| --- | --- | --- |
| Grid as the primary organizer | Keep the full-width search field, results, stats, and utility panels on visible column rules | Do not let asymmetry become arbitrary; every block still snaps to the grid |
| Functional geometry | Use circles, squares, triangles, lines, and grids as a deliberate composition with proportion, rhythm, and contrast | No arbitrary isolated shapes or accidental clipping; geometry must never be the only state indicator |
| Asymmetric composition | Give the query/search result area the dominant block and offset supporting stats or saved filters | Preserve predictable tab order and responsive stacking |
| New Typography | Use a clean sans-serif, large display labels, small uppercase metadata, and a disciplined type scale | Keep body copy readable and do not use all-caps for paragraphs |
| Material honesty | Use flat surfaces, hairline rules, hard or tiny radii, and restrained shadows | Do not recreate the old green glow with gradients or neon effects |
| Motion as composition | Lift the right group clear before unfolding the tray on focus/click, then retract the tray before returning the group on outside click or Escape | Respect `prefers-reduced-motion`; no decorative perpetual animation |

## Proposed visual system

- **Structure:** 12-column desktop grid, 8px spacing rhythm, 1px rules, square or 2px corners, no glassmorphism. Dark mode is the only visual mode for this developer application.
- **Branding:** use a single large `searchcord` wordmark in the top header. Keep the upper-left rail free of duplicate branding, subtitles, trial banners, and decorative marks.
- **Navigation:** keep useful top-level destinations, but omit a redundant search or results destination when the home surface owns search and reveals results. Do not add a server sidebar. Make active destinations clear and keep labels visible.
- **Browse:** use the available width for the full-width search banner. Place separate, compact messages-indexed and active-server stat boxes immediately below it.
- **Search:** the deep-navy banner spans the page. Use large transparent cutout circles and triangles, supported by a right-side square and bar, with angled horizontal and vertical rules forming an open grid behind the controls. Reserve clear bays for the top-left shapes, search copy, right group, and lower forms. Rules remain visible inside transparent shapes but pause at a navy clearance around outlines; keep all outlines and complete circles inside the banner, and keep the copy and controls unobstructed. Focusing the query lifts the right group before the lower filter tray appears; outside click or Escape reverses the movement. Use lavender, mint, pale blue, and selective muted gold. Server, channel, and author filters remain scalable text inputs with autocomplete. Result rows omit a redundant “view message” action and use the freed space for readable message text.
- **Live:** treat the feed as a time-indexed poster/list. The live indicator is a circle plus the word “live,” not color alone.
- **Stats:** use open chart panels with strong axes and labels. Keep the data legible before adding geometric decoration.
- **Settings:** preserve the token workflow and warnings, but make the drawer feel like a labeled utility sheet instead of a floating dark overlay.
- **Large counters:** format message totals compactly (`999k`, `1.2M`, `100M`) before placing them in the stat block so growth does not break the composition. Keep stat blocks content-sized at rest and let them reflow or stack when values require it.
- **Results pagination:** keep message rows small and readable at Discord-like density. When there is more than one page, use a restrained previous/next control with a current-page readout instead of a dense numbered control grid.
- **Message identity:** in the visual trial, give each result a randomized geometric profile avatar with enough variants to avoid adjacent repetition. The live implementation should use indexed Discord avatars when available and reserve the geometric avatar as a fallback.

## Privacy policy page

The static homepage trial links to `privacy-policy.html` from its footer at the user’s request. The destination is still a draft scaffold, not a completed policy. Writing the actual privacy policy is future work: confirm Searchcord’s collection, storage, retention, sharing, and user controls first, then write and review policy copy that describes those practices accurately.

## Color trials

These are deliberately separate static pages and use mock data only:

1. **Deep blue / lavender / green:** use a deep navy field, light lavender geometry, a mint-green wordmark accent, and restrained muted-gold details.

The trial pages do not load Dribbble assets, do not call Searchcord APIs, and do not contain tokens, scraped messages, or personal data. They are available locally at:

- `/trials/bauhaus-blue-purple.html`

### Trial revision

The first visual pass used a “find the signal” headline and a “recent signals” list. That framing was too decorative and too close to a landing-page slogan for an archive tool. The revised trial makes `searchcord` the large brand anchor, keeps “What are you looking for?” as the main action, removes slash-delimited labels and row numbering, and integrates the filters into the primary search block. Clicking or focusing the main search field expands the filter tray. Server, channel, and author filters are text inputs with datalist autocomplete, not dropdowns, so the pattern can scale to large indexed datasets. A colored view grid replaces the old top-level navigation treatment, and the message list begins directly below it.

### Banner composition pass (2026-09-22)

MoMA’s account of its 2009 Bauhaus exhibition website describes a stable grid that let visitors sort the works without changing the organizing structure. Getty’s Bauhaus teaching materials treat points, lines, planes, circles, squares, and triangles as forms to study through proportion, rhythm, and spatial relationships; they do not prescribe a universal color for each shape. Apply those ideas as a stable search layout with one composed group of forms, not as a fixed historical color code.

In the blue and purple trial, remove the scattered mini shapes and use larger circle-and-triangle groupings, supported by a square and bar at the right. Keep five angled rules across the banner and add two through the tilted lower-left group. Keep the forms behind the text and controls, with a visible blue clearance at each edge; the rules appear to break at that gap and continue beyond it. Preserve the main right-side group when the filter tray opens, shifting it upward on wider layouts and keeping its circle and triangle on narrow screens. Keep the deep-blue and light-lavender base with cool green accents; avoid yellow and warm colors. Honor reduced-motion preferences.

### Direction reset (2026-09-23, earlier pass)

Deepen the banner to near-navy and give `cord` a muted mint-green accent. That pass used translucent circles, squares, and triangles; the refinement below replaces those fills with consistent outline cutouts.

### Banner clarity and control refinement (2026-09-23)

Use one 3px full-opacity outline for every banner shape, with transparent interiors so the angled rules remain visible inside the forms. Leave a small navy gap where rules and outlines meet. Keep the lavender right circle as the treatment reference. Place the larger left shapes in a dedicated bay, preserve a clear column for “What are you looking for?” and the search field, keep the right group intact, and use the lower band for the enlarged irregular forms. Keep shapes fully inside the banner.

Use a porcelain fill with dark text and a restrained gold edge for the search and filter inputs; use mint for focus. Add a top color stripe to Browse, DMs, Live, and Stats, with larger geometric labels. Keep gold selective alongside the mint wordmark and lavender geometry.

The revised trial frames the main search input and button together with one border. Filter inputs each use one border; focus recolors that same border mint. Five subdued vertical rules join the angled horizontal rules as a background grid. Solid navy clearance keeps them away from text, controls, and shape outlines.

Open the filter tray with a short eased transition: the right forms lift first, then the tray unfolds. Keep the vertical rules visible behind the filter controls with navy clearance, keep the lower-left rules parallel through that group, and honor reduced-motion preferences.

Sources: [MoMA, Bauhaus: from Weimar to the Web](https://www.moma.org/explore/inside_out/2009/11/13/bauhaus-from-weimar-to-the-web/); [Getty Research Institute, Primary Forms](https://www.getty.edu/research/exhibitions_events/exhibitions/bauhaus/new_artist/form_color/form/).

## Validation and promotion plan

1. Compare the trial at desktop width, 768px, and 375px. Check whether the full-width banner geometry, stat boxes, query controls, and result rows remain clear when the grid collapses.
2. Test keyboard focus, form labels, reduced motion, and text contrast before touching the live app.
3. Choose one palette based on search-result readability and state clarity, not novelty.
4. Keep browse free of a duplicate results panel and results tab. Search submission reveals the results view and smoothly scrolls to it.
5. Extract semantic tokens into `static/style.css` and migrate one surface at a time: shell/navigation, browse, search, live, stats, settings.
6. Run the existing app checks and a local smoke test. Do not push the trial pages or any app changes to `main` until the direction is approved.
7. Keep the static trial’s footer link pointed at the local privacy-policy draft scaffold. Replace the scaffold with reviewed policy copy after verifying the app’s collection, storage, retention, sharing, and user-control behavior.

## Cleanup boundary

The repository already ignores `data/`, database files, JSONL exports, root JSON scrape output, and token-like local config. Before any future push, inspect `git status --short` and `git diff --stat`, then verify no tracked or staged file contains a personal Discord token, scraped message content, or generated export. The visual trials in this change are self-contained CSS/HTML and intentionally contain no external assets or live data.
