import { Component, inject } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router } from '@angular/router';
import { Session } from '../../core/session';

/**
 * Sign-in screen (RM-GA-P4 OBS-805): sets the tenant/principal/API-key the
 * dashboard acts as, replacing the previous hardcoded build-time constants.
 * There is no username/password login endpoint anywhere in the platform (see
 * `Session`'s doc comment for why) — this collects an already-minted API key
 * (`wovyr auth create-key <principal>`) or JWT instead.
 */
@Component({
  selector: 'app-login',
  imports: [FormsModule],
  templateUrl: './login.html',
  styleUrl: './login.scss',
})
export class Login {
  private session = inject(Session);
  private router = inject(Router);

  tenant = this.session.tenant();
  principal = this.session.principal();
  apiKey = this.session.apiKey();
  saved = false;

  save(): void {
    if (!this.tenant.trim() || !this.principal.trim()) return;
    this.session.save(this.tenant, this.principal, this.apiKey);
    this.saved = true;
    this.router.navigateByUrl('/monitoring');
  }
}
