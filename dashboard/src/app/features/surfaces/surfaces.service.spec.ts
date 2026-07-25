import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SurfacesService, UiFrame } from './surfaces.service';

/** RM-GUI-P3 EMB-701: pins the exact request shape this service sends against
 * the standalone middleware routes — a drifted method/URL/body here would
 * silently 404/400 against a real `wovyr-server` without any type error. */
describe('SurfacesService', () => {
  let service: SurfacesService;
  let http: HttpTestingController;

  beforeEach(() => {
    TestBed.configureTestingModule({
      providers: [provideHttpClient(), provideHttpClientTesting()],
    });
    service = TestBed.inject(SurfacesService);
    http = TestBed.inject(HttpTestingController);
  });

  afterEach(() => http.verify());

  const frame: UiFrame = {
    schema_version: '1.0.0',
    title: 'Confirm refund',
    root: { type: 'column', children: [] },
  };

  it('present() POSTs the frame to /api/v1/ui/present', () => {
    service.present(frame).subscribe();
    const req = http.expectOne('/api/v1/ui/present');
    expect(req.request.method).toBe('POST');
    expect(req.request.body).toEqual({ frame });
    req.flush({
      frame_id: 'uif-1',
      execution_id: null,
      activity_id: null,
      frame,
      frame_hash: 'abc123',
      policy_ref: 'default@v1',
      created_at_ms: 0,
    });
  });

  it('decide() POSTs the action/values to /api/v1/ui/decisions/{frame_id}, URL-encoded', () => {
    service.decide('uif/weird id', 'approve', { note: 'ok' }).subscribe();
    const req = http.expectOne('/api/v1/ui/decisions/uif%2Fweird%20id');
    expect(req.request.method).toBe('POST');
    expect(req.request.body).toEqual({ action: 'approve', values: { note: 'ok' } });
    req.flush({ frame_id: 'uif/weird id', execution_id: null, activity_id: null, status: 'decided' });
  });

  it('decide() defaults values to {} when omitted', () => {
    service.decide('uif-1', 'cancel').subscribe();
    const req = http.expectOne('/api/v1/ui/decisions/uif-1');
    expect(req.request.body).toEqual({ action: 'cancel', values: {} });
    req.flush({ frame_id: 'uif-1', execution_id: null, activity_id: null, status: 'decided' });
  });

  it('getDecision() GETs /api/v1/ui/decisions/{frame_id}', () => {
    service.getDecision('uif-1').subscribe();
    const req = http.expectOne('/api/v1/ui/decisions/uif-1');
    expect(req.request.method).toBe('GET');
    req.flush({
      frame_id: 'uif-1',
      action: 'approve',
      values: {},
      decided_by: 'operator',
      decided_at_ms: 0,
      frame_hash: 'abc123',
    });
  });
});
