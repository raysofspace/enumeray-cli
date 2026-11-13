;; Normalized transmission of a two-dimensional dielectric waveguide.
;; Geometry follows the Meep waveguide-cavity tutorial and Fan et al. (1995).
(define-param spacing 1.4)
(define-param grid 16)
(define-param reference? false)

(set! resolution grid)
(set! geometry-lattice (make lattice (size 14 6 no-size)))
(set! pml-layers (list (make pml (thickness 1))))
(set! symmetries (list (make mirror-sym (direction Y) (phase -1))))
(set! filename-prefix "cavity")

(define guide
  (make block (center 0 0) (size infinity 1.2 infinity)
    (material (make dielectric (epsilon 13)))))
(define (air-hole offset)
  (make cylinder (center offset 0) (radius 0.36) (height infinity) (material air)))
(define half-spacing (/ spacing 2))
(define distances (list half-spacing (+ half-spacing 1) (+ half-spacing 2)))
(set! geometry
  (if reference? (list guide)
      (cons guide (map air-hole (append distances (map - distances))))))

(set! sources
  (list (make source (src (make gaussian-src (frequency 0.25) (fwidth 0.2)))
    (component Ey) (center -5.5 0) (size 0 1.2))))
(define transmission-flux
  (add-flux 0.235 0.08 401 (make flux-region (center 5.5 0) (size 0 2.4))))
(define decayed (stop-when-fields-decayed 50 Ey (vector3 5.5 0) 0.003))
(if reference?
    (run-sources+ decayed)
    (run-sources+ decayed (at-beginning output-epsilon) (at-time 200 output-hfield-z)))
(display-fluxes transmission-flux)
