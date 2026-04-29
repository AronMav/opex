/**
 * Shared IntersectionObserver mock for tail-sentinel tests.
 *
 * Exported so both the unit tests (tail-sentinel.test.ts) and the
 * integration tests (tail-sentinel-integration.test.tsx) can reuse
 * the same implementation without duplication.
 *
 * Usage:
 *   beforeEach(() => {
 *     MockIntersectionObserver.instances = [];
 *     vi.stubGlobal("IntersectionObserver", MockIntersectionObserver);
 *   });
 *   afterEach(() => vi.unstubAllGlobals());
 */

export class MockIntersectionObserver {
  static instances: MockIntersectionObserver[] = [];

  readonly callback: IntersectionObserverCallback;
  readonly options: IntersectionObserverInit | undefined;
  readonly observed: Element[] = [];
  disconnected = false;

  constructor(
    cb: IntersectionObserverCallback,
    options?: IntersectionObserverInit,
  ) {
    this.callback = cb;
    this.options = options;
    MockIntersectionObserver.instances.push(this);
  }

  observe(el: Element) {
    this.observed.push(el);
  }

  unobserve() {}

  disconnect() {
    this.disconnected = true;
  }

  takeRecords(): IntersectionObserverEntry[] {
    return [];
  }

  /** Drive an `isIntersecting` transition from the test. */
  fire(isIntersecting: boolean) {
    const entry = {
      isIntersecting,
      target: this.observed[0],
      intersectionRatio: isIntersecting ? 1 : 0,
    } as unknown as IntersectionObserverEntry;
    this.callback([entry], this as unknown as IntersectionObserver);
  }

  static last(): MockIntersectionObserver {
    return MockIntersectionObserver.instances.at(-1)!;
  }
}
