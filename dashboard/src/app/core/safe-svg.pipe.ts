import { Pipe, PipeTransform, inject } from '@angular/core';
import { DomSanitizer, SafeHtml } from '@angular/platform-browser';

/**
 * Marks a trusted, in-repo SVG markup string as safe for `[innerHTML]`. The values are
 * static icon definitions authored in this codebase — never user input — so bypassing
 * sanitization here is safe.
 */
@Pipe({ name: 'safeSvg' })
export class SafeSvgPipe implements PipeTransform {
  private sanitizer = inject(DomSanitizer);
  transform(svg: string): SafeHtml {
    return this.sanitizer.bypassSecurityTrustHtml(svg);
  }
}
