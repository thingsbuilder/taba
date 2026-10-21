import { CHILD_STATUSES, DISABILITIES, type FormError, NEED_KEYS, type PersonForm } from '../checkin'
import { t } from '../i18n'

/** The data fields of a person (spec 3.4), shared by the check-in and the edit of a person. */
export function PersonFields({ form, onChange, errors = [], showChild = true }: {
  form: PersonForm
  onChange: (form: PersonForm) => void
  errors?: FormError[]
  showChild?: boolean
}) {
  const set = <K extends keyof PersonForm>(key: K, value: PersonForm[K]) => onChange({ ...form, [key]: value })
  const text = (key: 'name' | 'social_name' | 'wristband' | 'birth_year', label: string, extra: Record<string, string> = {}) => (
    <label class="field">
      {label}
      <input type="text" value={form[key]} aria-invalid={errors.includes(key as FormError)} onInput={(e) => set(key, e.currentTarget.value)} {...extra} />
    </label>
  )
  return (
    <div class="form">
      {text('name', t('checkin.name'), { autocomplete: 'off', maxLength: '100' })}
      {text('social_name', t('checkin.social_name'), { autocomplete: 'off', maxLength: '100' })}
      {text('wristband', t('checkin.wristband'), { autocomplete: 'off', maxLength: '20' })}
      {errors.includes('name_or_wristband') && <p class="notice notice-error" role="alert">{t('checkin.err_name_or_wristband')}</p>}
      {text('birth_year', t('checkin.birth_year'), { inputMode: 'numeric', maxLength: '4' })}
      {errors.includes('birth_year') && <p class="notice notice-error" role="alert">{t('checkin.err_birth_year')}</p>}

      {showChild && (
        <>
          <label class="check">
            <input type="checkbox" checked={form.is_child} onChange={(e) => onChange({ ...form, is_child: e.currentTarget.checked, child_status: e.currentTarget.checked ? form.child_status : '' })} />
            {t('checkin.is_child')}
          </label>
          {form.is_child && (
            <fieldset class="card fieldset">
              <legend class="body-strong">{t('checkin.child_status')}</legend>
              {CHILD_STATUSES.map((s) => (
                <label class="check" key={s}>
                  <input type="radio" name="child_status" checked={form.child_status === s} onChange={() => set('child_status', s)} />
                  {t(`child.status.${s}`)}
                </label>
              ))}
              {errors.includes('child_status') && <p class="notice notice-error" role="alert">{t('checkin.err_child_status')}</p>}
            </fieldset>
          )}
        </>
      )}

      <fieldset class="card fieldset">
        <legend class="body-strong">{t('checkin.needs_title')}</legend>
        <p class="caption">{t('checkin.needs_hint')}</p>
        {NEED_KEYS.map((k) => (
          <label class="check" key={k}>
            <input type="checkbox" checked={form.needs[k]} onChange={(e) => set('needs', { ...form.needs, [k]: e.currentTarget.checked })} />
            {t(`checkin.need.${k}`)}
          </label>
        ))}
      </fieldset>

      <details class="card">
        <summary class="body-strong">{t('checkin.vulnerabilities_title')}</summary>
        <label class="check"><input type="checkbox" checked={form.pregnant} onChange={(e) => set('pregnant', e.currentTarget.checked)} />{t('checkin.pregnant')}</label>
        <label class="check"><input type="checkbox" checked={form.lactating} onChange={(e) => set('lactating', e.currentTarget.checked)} />{t('checkin.lactating')}</label>
        <p class="body-strong">{t('checkin.disability_title')}</p>
        {DISABILITIES.map((d) => (
          <label class="check" key={d}>
            <input
              type="checkbox"
              checked={form.disability.includes(d)}
              onChange={(e) => set('disability', e.currentTarget.checked ? [...form.disability, d] : form.disability.filter((x) => x !== d))}
            />
            {t(`checkin.disability.${d}`)}
          </label>
        ))}
        <label class="field">
          {t('checkin.indigenous_people')}
          <input type="text" maxLength={100} value={form.indigenous_people} onInput={(e) => set('indigenous_people', e.currentTarget.value)} />
        </label>
        <label class="field">
          {t('checkin.migrant_language')}
          <input type="text" maxLength={100} value={form.migrant_language} onInput={(e) => set('migrant_language', e.currentTarget.value)} />
        </label>
      </details>
    </div>
  )
}
