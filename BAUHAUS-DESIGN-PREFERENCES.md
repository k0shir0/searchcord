# Searchcord Bauhaus design preferences

Read this before changing the Searchcord visual trials or proposing a new visual direction.

## Non-negotiables

- Searchcord is a developer application. Functionality comes before decoration.
- Search belongs on the home surface. Do not add a separate search tab just because search is important.
- The main search field owns the filters. Clicking or focusing it expands the filter tray with a visible animation; clicking outside or pressing Escape collapses the tray back into the search surface.
- Use one primary `searchcord` wordmark in the large top header. Do not repeat the brand in a sidebar lockup, add a “message archive” subtitle, or put a trial-status strip above it.
- The blue search banner spans the full page width. Put the messages-indexed and active-server totals in two compact, separate boxes directly beneath it.
- Remove the server sidebar. Keep server selection in the search filters and use the freed width for the full-width banner and results.
- Server, channel, and author filters must be text inputs with autocomplete from indexed data. Dropdowns do not scale to the expected number of servers, channels, or users.
- Preserve the real search filters and result behavior when a visual direction is promoted from the static trial.
- Dark mode is the default and only mode for this developer tool. Do not add a light/dark toggle.

## Likes

- The standalone `searchcord` hero is stronger than a slogan. Give the wordmark a little breathing room in its lettering.
- Bauhaus should be visible as structure: geometry, clear rules, strong blocks, asymmetry with alignment, and intentional composition.
- Geometry is welcome when it is composed as one deliberate system. Keep it restrained and purposeful, not as isolated dots or floating shapes.
- Search-box geometry should use clear alignment without a straight grid. Circles should remain complete inside their reserved bay, sized as large as possible without clipping or competing with the headline and form.
- Fill the blue banner with a few large, related circles and triangles, supported by a square and bar where the layout has room, plus angled rules. Do not use scattered mini ornaments or a straight grid. Let the forms sit behind the headline, search field, and filters with a clear banner-colored gap at each edge. Rules should appear to stop at the gap and resume on the far side. Keep circles whole. Use substantial left and center groups to balance the main right group; on narrow screens simplify the layout while keeping the main circle and triangle visible when search opens.
- Keep the dark-blue and light-purple direction, with pale blue and marigold accents. Avoid the coral-red and white pairing.
- Color should reach across the page through meaningful surfaces, tabs, stats, and states instead of random decoration.
- The result area should remain. It represents the messages revealed by search and sorting, and should be labelled `results`.
- On the browse surface, do not render a redundant results box or a results tab. Submitting the main search reveals the results view and smoothly scrolls there.
- The hero lettering should be close together again, with only the measured breathing room used by the `.4` in the indexed counter. Search-box geometry belongs in a complete reserved visual bay and must not sit behind the headline or form text.
- Results may span many pages. Use a composed previous/next paginator with a clear current-page readout and no cluttered grid of unexplained page-number buttons.
- Message rows should stay close to Discord message density. Give each row a randomized, non-repeating geometric avatar treatment in the trial; the promoted product should prefer a real Discord avatar and use the geometric treatment as a fallback.
- Do not show a “view message” action on a result that is already open in the results list. Use that space to keep the message copy readable and comfortably spaced.
- Indexed totals and active-server stats must be content-driven and compact at rest. Let their boxes grow, reflow, or stack when values need room instead of reserving oversized empty panels.

## Dislikes and removals

- Do not use “find the signal.”
- Do not use “recent signals.”
- Do not use slash-delimited labels such as `text / text`.
- Do not use unexplained numeric labels on navigation or buttons.
- Do not leave empty decorative sections under the hero.
- Do not use random dots or ungrouped geometric shapes in the top corners.
- Do not add a server sidebar.
- Do not add redundant “view message” controls to visible results.
- Do not separate the search archive from the primary search bar.
- Do not add a separate `search` tab when the home surface already searches.

## Interaction expectations

- The filter tray should open from the bottom of the main search surface, animate in, and return into that surface when dismissed.
- Submitting the main search should activate and scroll to results. The browse view should remain focused on the search surface until that action.
- Autocomplete suggestions in static trials may be mock data, but the promoted implementation must populate them from Searchcord’s indexed data.
- Keep controls keyboard accessible, honor reduced-motion preferences, and preserve the search workflow. Style search focus according to the user’s current direction.
- Include a `privacy policy` text link in the static homepage trial footer. Keep the linked local page identified as a draft scaffold until policy copy is written from verified collection, storage, retention, sharing, and user-control behavior.

## Banner repair notes (2026-09-22)

- Recent Bauhaus form research supports using circles, squares, triangles, lines, planes, and grids as compositional material. Arrange them with proportion, rhythm, contrast, and intentional overlap; do not scatter isolated ornaments or leave unexplained fragments.
- Use five angled rules across the banner plus two angled rules through the lower-left group; do not use a straight grid. Remove isolated mini ornaments. Keep the lower-left circle-and-triangle group visibly tilted and irregular, with the rules passing behind it and stopping at a clear banner-colored gap around each form. Leave the same visible gap around text and controls. Keep circles whole. Keep the main right-side group visible when search opens, and use lower-left and center groups to continue the composition. On narrow screens, retain the right circle and triangle while simplifying the extra groups.
- Focusing/clicking the main query unfolds the filter tray from the banner’s lower edge, and clicking outside or pressing Escape returns the banner and its forms to their resting positions. Keep the motion tied to that interaction and honor reduced-motion preferences.
- Bauhaus history does not mandate one color per shape. Dark blue and light purple remain the base palette; pale blue and marigold are contemporary accents, not claimed historical rules. Do not reintroduce the coral-red accent.
- Keep geometry motion subtle and coordinated as one layer. The banner has no purple focus border and the search field has no outline on focus. Match the search and clear-filter buttons: blue outlined at rest, then invert to cream and dark ink when hovered, pressed, or focused. Center the wordmark and distinguish one segment with an accent color; keep the un-underlined privacy link at the bottom.
