import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ConfirmDialog, ConfirmService } from './confirm';
import { statusClass } from './status-pill';
import { Tabs, TabSpec } from './tabs';

/** UI-301: the shared component library's contracts. */
describe('statusClass', () => {
  it('maps every engine status family to its pill class, wire-casing included', () => {
    // The engine serializes snake_case (`completed`, API-702); the old
    // PascalCase is still accepted for anything that stored it.
    expect(statusClass('completed')).toBe('ok');
    expect(statusClass('Completed')).toBe('ok');
    expect(statusClass('failed')).toBe('crit');
    expect(statusClass('compensating')).toBe('warn');
    for (const s of ['running', 'waiting', 'resumed', 'scheduled', 'Running']) {
      expect(statusClass(s)).toBe('info');
    }
    expect(statusClass('something_new')).toBe('mut');
  });
});

describe('ConfirmService', () => {
  let svc: ConfirmService;

  beforeEach(() => {
    TestBed.configureTestingModule({});
    svc = TestBed.inject(ConfirmService);
  });

  it('resolves true on confirm and false on cancel', async () => {
    const a = svc.ask({ message: 'Uninstall x?' });
    svc.settle(true);
    expect(await a).toBeTrue();

    const b = svc.ask({ message: 'Uninstall y?' });
    svc.settle(false);
    expect(await b).toBeFalse();
    expect(svc.pending()).toBeNull();
  });

  it('auto-cancels a superseded request instead of dropping its resolver', async () => {
    const first = svc.ask({ message: 'one' });
    const second = svc.ask({ message: 'two' });
    svc.settle(true);
    expect(await first).toBeFalse();
    expect(await second).toBeTrue();
  });
});

describe('ConfirmDialog', () => {
  it('renders the pending request and settles through its buttons', async () => {
    TestBed.configureTestingModule({ imports: [ConfirmDialog] });
    const svc = TestBed.inject(ConfirmService);
    const fixture = TestBed.createComponent(ConfirmDialog);
    fixture.detectChanges();

    const answer = svc.ask({ message: 'Uninstall demo?', confirmLabel: 'Uninstall', danger: true });
    fixture.detectChanges();
    const el: HTMLElement = fixture.nativeElement;
    expect(el.textContent).toContain('Uninstall demo?');

    const buttons = Array.from(el.querySelectorAll<HTMLButtonElement>('.actions button'));
    expect(buttons.map((b) => b.textContent?.trim())).toEqual(['Cancel', 'Uninstall']);
    buttons[1].click();
    expect(await answer).toBeTrue();
    fixture.detectChanges();
    expect(el.querySelector('[role="dialog"]')).toBeNull();
  });
});

@Component({
  imports: [Tabs],
  template: `<app-tabs [tabs]="tabs" [active]="active()" (activeChange)="active.set($event)" />`,
})
class TabsHost {
  tabs: TabSpec[] = [
    { id: 'a', label: 'Alpha' },
    { id: 'b', label: 'Beta', count: 3 },
  ];
  active = signal('a');
}

describe('Tabs', () => {
  it('marks the active tab, shows counts, and switches on click', () => {
    TestBed.configureTestingModule({ imports: [TabsHost] });
    const fixture = TestBed.createComponent(TabsHost);
    fixture.detectChanges();
    const el: HTMLElement = fixture.nativeElement;

    const tabs = Array.from(el.querySelectorAll<HTMLButtonElement>('[role="tab"]'));
    expect(tabs.length).toBe(2);
    expect(tabs[0].getAttribute('aria-selected')).toBe('true');
    expect(tabs[1].textContent).toContain('3');

    tabs[1].click();
    fixture.detectChanges();
    expect(fixture.componentInstance.active()).toBe('b');
    expect(tabs[1].getAttribute('aria-selected')).toBe('true');
  });

  it('moves selection with arrow keys (roving tabindex)', () => {
    TestBed.configureTestingModule({ imports: [TabsHost] });
    const fixture = TestBed.createComponent(TabsHost);
    fixture.detectChanges();
    const tabs = Array.from(
      (fixture.nativeElement as HTMLElement).querySelectorAll<HTMLButtonElement>('[role="tab"]'),
    );

    tabs[0].dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
    fixture.detectChanges();
    expect(fixture.componentInstance.active()).toBe('b');
    // wraps past the end
    tabs[1].dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
    fixture.detectChanges();
    expect(fixture.componentInstance.active()).toBe('a');
  });
});
