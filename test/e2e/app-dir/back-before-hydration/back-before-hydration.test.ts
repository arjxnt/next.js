import { nextTestSetup } from 'e2e-utils'
import { retry } from 'next-test-utils'
import type * as Playwright from 'playwright'

// Reproduces a URL/content desync when the browser's Back button is pressed
// while a reloaded page has committed but not yet hydrated.
//
// Setup: navigate client-side so both history entries are same-document
// entries created via pushState, then reload. Chrome preserves the
// same-document association across a reload, so a Back traversal that happens
// before the new document hydrates is an *instant same-document* traversal:
// the URL bar changes and popstate fires, but no router is attached yet, so
// the rendered content stays on the reloaded page. When hydration then
// completes, the router assumes the current URL matches its payload and calls
// replaceState() with the payload tree onto the traversed entry. From that
// point the URL bar and the content permanently disagree, and subsequent
// Back/Forward traversals update the URL bar without ever changing what is
// rendered.
//
// In manual testing this is easiest to hit with a hard reload (shift+cmd+R)
// because the widened commit-to-hydration window makes the race human-sized;
// here we make it deterministic by stalling the static scripts instead.
describe('back navigation before hydration after reload', () => {
  const { next } = nextTestSetup({ files: __dirname })

  // Navigates client-side (creating a same-document sibling entry), reloads
  // with all static scripts stalled so the new document commits but cannot
  // start hydrating, presses Back inside that window, then releases the
  // scripts so hydration proceeds. (Routing a pattern also disables the
  // browser HTTP cache for it, so cached scripts are stalled too.)
  async function clickThenBackBeforeHydration(
    startPath: string,
    linkId: string,
    headingAfterClick: string
  ) {
    let page: Playwright.Page
    const browser = await next.browser(startPath, {
      beforePageLoad(p: Playwright.Page) {
        page = p
      },
    })

    await browser.elementById(linkId).click()
    await retry(async () => {
      expect(await browser.elementByCss('h1').text()).toBe(headingAfterClick)
    })

    let stallScripts = true
    const stalled: Array<() => void> = []
    await page.route('**/_next/static/**', async (route) => {
      if (stallScripts && route.request().resourceType() === 'script') {
        await new Promise<void>((resolve) => stalled.push(resolve))
      }
      await route.continue()
    })

    // Reload, returning as soon as the new document commits, then go Back
    // while it is not hydrated: an instant same-document traversal handled
    // by nobody.
    await browser.refresh({ waitUntil: 'commit' })
    await browser.back({ waitUntil: 'commit' })

    stallScripts = false
    for (const release of stalled) release()

    return browser
  }

  it('reconciles the URL with the rendered content once hydration completes', async () => {
    const browser = await clickThenBackBeforeHydration('/', 'to-post', 'Post')

    // We traversed back to '/', so once the router is up it must render the
    // home page (or otherwise bring URL and content back in sync).
    await retry(async () => {
      expect(new URL(await browser.url()).pathname).toBe('/')
      expect(await browser.elementByCss('h1').text()).toBe('Home')
    })

    // History traversal must still work after recovery.
    await browser.forward()
    await retry(async () => {
      expect(new URL(await browser.url()).pathname).toBe('/post')
      expect(await browser.elementByCss('h1').text()).toBe('Post')
    })

    await browser.back()
    await retry(async () => {
      expect(new URL(await browser.url()).pathname).toBe('/')
      expect(await browser.elementByCss('h1').text()).toBe('Home')
    })
  })

  it('reconciles when the traversed entry differs only in search params', async () => {
    const browser = await clickThenBackBeforeHydration(
      '/search?page=1',
      'to-page-2',
      'Page 2'
    )

    await retry(async () => {
      expect(new URL(await browser.url()).search).toBe('?page=1')
      expect(await browser.elementByCss('h1').text()).toBe('Page 1')
    })

    await browser.forward()
    await retry(async () => {
      expect(new URL(await browser.url()).search).toBe('?page=2')
      expect(await browser.elementByCss('h1').text()).toBe('Page 2')
    })
  })
})
