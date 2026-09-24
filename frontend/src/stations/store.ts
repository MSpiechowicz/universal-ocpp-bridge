import type { ApiClient } from '../http';
import type { StationPage, StationSnapshot } from './schema';

export interface InventoryState {
  page?: StationPage;
  selected?: string;
  detail?: StationSnapshot;
  stale: boolean;
  loading: boolean;
  moreLoading: boolean;
  error?: string;
}

// One serialized read lane keeps pagination and selection below the client's two-read ceiling.
// A generation makes a late response from a previous selection or page invisible.
export class StationStore {
  private queue: Promise<void> = Promise.resolve();
  private generation = 0;
  private revision = 0;
  private refreshQueued = false;
  private connected = false;
  private stopped = false;
  private snapshot: InventoryState = { stale: true, loading: false, moreLoading: false };
  constructor(private readonly api: ApiClient, private readonly publish: (value: InventoryState) => void) {}

  get state(): InventoryState { return this.snapshot; }
  private update(change: Partial<InventoryState>) {
    if (this.stopped) return;
    this.snapshot = { ...this.snapshot, ...change };
    this.publish(this.snapshot);
  }
  initialize(page: StationPage) { this.update({ page }); }
  private enqueue(job: () => Promise<void>) {
    this.queue = this.queue.catch(() => {}).then(async () => {
      if (!this.stopped) await job();
    });
  }
  close() { this.stopped = true; this.generation++; }

  select(id: string) {
    if (this.snapshot.selected === id) return;
    // A new selection invalidates the old stream before React has replaced its subscription.
    this.connected = false;
    this.revision++;
    const generation = ++this.generation;
    this.update({ selected: id, detail: undefined, loading: true, error: undefined, stale: true });
    this.enqueue(async () => {
      if (generation !== this.generation) return;
      try {
        const detail = await this.api.station(id);
        if (generation === this.generation) this.update({ detail, loading: false, error: undefined });
      } catch {
        if (generation === this.generation) this.update({ loading: false, error: 'Station detail is unavailable. Data remains stale.' });
      }
    });
  }

  more() {
    const cursor = this.snapshot.page?.next_cursor;
    if (!cursor || this.snapshot.moreLoading || (this.snapshot.page?.items.length ?? 0) >= 100) return;
    const generation = this.generation;
    const revision = this.revision;
    this.update({ moreLoading: true, error: undefined });
    this.enqueue(async () => {
      if (generation !== this.generation || revision !== this.revision) { this.update({ moreLoading: false }); return; }
      try {
        const next = await this.api.stations(cursor);
        if (generation !== this.generation || revision !== this.revision) { this.update({ moreLoading: false }); return; }
        const items = [...this.snapshot.page!.items];
        const ids = new Set(items.map(item => item.station.station_id));
        for (const item of next.items) {
          if (!ids.has(item.station.station_id) && items.length < 100) {
            items.push(item); ids.add(item.station.station_id);
          }
        }
        const advanced = items.length > this.snapshot.page!.items.length && next.next_cursor !== cursor;
        this.update({ page: { items, next_cursor: advanced && items.length < 100 ? next.next_cursor : undefined }, moreLoading: false });
      } catch {
        this.update(generation === this.generation && revision === this.revision
          ? { moreLoading: false, error: 'Next inventory page is unavailable.' } : { moreLoading: false });
      }
    });
  }

  stream(connected: boolean) {
    this.connected = connected;
    if (!connected) { this.revision++; this.update({ stale: true }); }
    else this.refresh();
  }
  recovered(detail: StationSnapshot) {
    if (this.snapshot.selected === detail.station.station_id) this.update({ detail, stale: true });
    this.refresh();
  }
  refresh() {
    this.revision++;
    this.update({ stale: true });
    if (this.refreshQueued) return;
    this.refreshQueued = true;
    this.enqueue(async () => {
      this.refreshQueued = false;
      const generation = this.generation;
      const revision = this.revision;
      this.update({ loading: true, error: undefined });
      try {
        const page = await this.api.stations();
        if (generation !== this.generation || revision !== this.revision) { this.refresh(); return; }
        const id = this.snapshot.selected;
        const detail = id ? await this.api.station(id) : undefined;
        if (generation !== this.generation || revision !== this.revision) { this.refresh(); return; }
        // Refresh restarts at page one rather than retaining an invalidated opaque cursor.
        this.update({ page, detail, stale: !this.connected, loading: false, error: undefined });
      } catch {
        if (generation !== this.generation || revision !== this.revision) this.refresh();
        else this.update({ stale: true, loading: false, error: 'Could not refresh station state. Previous observations remain stale.' });
      }
    });
  }
}
