{{/*
Chart name, truncated/sanitized for use in Kubernetes object names.
*/}}
{{- define "wovyr.name" -}}
{{- .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Fully qualified app name: <release>-wovyr, or just the release name if it
already contains "wovyr" (standard Helm chart convention).
*/}}
{{- define "wovyr.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "wovyr.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Common labels, applied to every object this chart renders. Deliberately does
NOT include the name/instance selector labels — every template composes
those separately via one of the *.selectorLabels helpers below, so including
them here too would set the same map keys twice in the same YAML document.
*/}}
{{- define "wovyr.labels" -}}
helm.sh/chart: {{ include "wovyr.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{/*
Selector labels for the `wovyr` server component specifically.
*/}}
{{- define "wovyr.selectorLabels" -}}
app.kubernetes.io/name: {{ include "wovyr.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "wovyr.server.selectorLabels" -}}
{{ include "wovyr.selectorLabels" . }}
app.kubernetes.io/component: server
{{- end -}}

{{- define "wovyr.postgres.selectorLabels" -}}
{{ include "wovyr.selectorLabels" . }}
app.kubernetes.io/component: postgres
{{- end -}}

{{- define "wovyr.qdrant.selectorLabels" -}}
{{ include "wovyr.selectorLabels" . }}
app.kubernetes.io/component: qdrant
{{- end -}}

{{/*
The postgres DSN wovyr-server/wovyr-cli expect, built from values.yaml's
postgres.* fields and this chart's postgres Service DNS name — used for both
WOVYR_MARKETPLACE_POSTGRES_URL and WOVYR_MEMORY_POSTGRES_URL (same database,
same convention deployment/docker-compose.yml uses).
*/}}
{{- define "wovyr.postgresDsn" -}}
{{- printf "postgres://%s:%s@%s-postgres:5432/%s" .Values.postgres.user .Values.postgres.password (include "wovyr.fullname" .) .Values.postgres.database -}}
{{- end -}}

{{- define "wovyr.qdrantUrl" -}}
{{- printf "http://%s-qdrant:6333" (include "wovyr.fullname" .) -}}
{{- end -}}
